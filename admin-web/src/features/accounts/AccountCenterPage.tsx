import { useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useApi } from "../../hooks/useApi";
import { useToast } from "../../context/ToastContext";
import { useAuth } from "../../context/AuthContext";
import { can } from "../../lib/permissions";
import {
  api,
  commitAccountsImport,
  previewAccountsImport,
  previewOutlookAccounts,
  syncOutlookAccounts,
} from "../../lib/api";
import {
  AccountImportMode,
  AccountImportPreview,
  AccountListResponse,
  ApiError,
  OutlookPreviewResponse,
  type Account,
  type ResetQuotaResponse,
} from "../../lib/types";
import { formatTime } from "../../lib/format";
import { MailProviderStatus } from "./MailProviderStatus";
import {
  Button,
  Card,
  Dialog,
  EmptyRow,
  ErrorBox,
  Input,
  Spinner,
  StatusBadge,
  Table,
  Td,
} from "../../components/ui";
import {
  FileUp,
  Plus,
  Users,
  Clock,
  AlertCircle,
  ListOrdered,
  ArrowRight,
  ShieldCheck,
  ChevronLeft,
  ChevronRight,
  RefreshCw,
} from "lucide-react";

export function AccountCenterPage() {
  const PAGE_SIZE = 20;
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_account");
  const isSuperAdmin = user?.role === "超级管理员";
  const [page, setPage] = useState(1);
  const { data, loading, error, reload } = useApi<AccountListResponse>(
    () => api.get("/api/accounts", { limit: PAGE_SIZE, offset: (page - 1) * PAGE_SIZE }),
    [page],
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
  const [regBatchName, setRegBatchName] = useState("");
  const [regPriority, setRegPriority] = useState("10");
  const [startImmediately, setStartImmediately] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [previewData, setPreviewData] = useState<AccountImportPreview | null>(null);
  const [committing, setCommitting] = useState(false);
  const [resettingQuota, setResettingQuota] = useState(false);

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

  const resetQuota = async () => {
    setResettingQuota(true);
    try {
      const res = await api.post<ResetQuotaResponse>("/api/accounts/reset-quota");
      toast.success(res.message);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "重置失败");
    } finally {
      setResettingQuota(false);
    }
  };

  const handleOpenImportWizard = () => {
    setImportStep("select");
    setSelectedFile(null);
    setImportMode("待注册");
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
        create_registration_batch: importMode === "待注册",
        batch_name: regBatchName.trim() || undefined,
        priority: Number(regPriority),
        start_immediately: startImmediately,
      });

      let msg = `已成功导入 ${res.imported_accounts} 个${importMode}账号`;
      if (res.registration_batch) {
        msg += `，并自动加入注册队列「${res.registration_batch.name}」`;
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

  if (loading) return <Spinner label="正在加载账号资源池..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  const accounts = data?.items ?? [];
  const summary = data?.summary;
  const total = summary?.total ?? data?.total ?? 0;
  const registered = summary?.registered ?? 0;
  const available = summary?.available ?? 0;
  const pendingReg = summary?.pending_registration ?? 0;
  const disabled = summary?.disabled ?? 0;
  const limitReached = summary?.exhausted_today ?? 0;
  const totalPages = Math.max(1, Math.ceil((data?.total ?? 0) / PAGE_SIZE));

  return (
    <div className="space-y-6">
      {/* 顶部概览与操作 */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-slate-900">账号中心</h1>
          <p className="text-xs text-slate-500">
            全生命周期账号资源池：可用下载账号、注册队列与自动化验证
          </p>
        </div>
        <div className="flex items-center gap-2">
          {canManage && (
            <Button variant="secondary" size="sm" loading={resettingQuota} onClick={resetQuota}>
              <RefreshCw className="mr-1.5 h-4 w-4" />
              重置额度耗尽账号
            </Button>
          )}
          <Link to="/accounts/registrations">
            <Button variant="secondary" size="sm">
              <ListOrdered className="mr-1.5 h-4 w-4 text-blue-600" />
              注册队列
            </Button>
          </Link>
          {isSuperAdmin && (
            <Button variant="secondary" size="sm" onClick={handleOpenOutlookWizard}>
              <RefreshCw className="mr-1.5 h-4 w-4 text-blue-600" />
              同步 Outlook 账号
            </Button>
          )}
          {isSuperAdmin && (
            <Button variant="secondary" size="sm" onClick={handleOpenImportWizard}>
              <FileUp className="mr-1.5 h-4 w-4" />
              批量导入账号
            </Button>
          )}
          {canManage && (
            <Button size="sm" onClick={() => setCreating(true)}>
              <Plus className="mr-1.5 h-4 w-4" />
              新增账号
            </Button>
          )}
        </div>
      </div>

      {/* 邮件验证码服务 Provider 实时状态 */}
      <MailProviderStatus />

      {/* 账号池概况卡片 */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Card className="p-4">
          <div className="flex items-center justify-between text-slate-500 text-xs">
            <span>总账号数</span>
            <Users className="h-4 w-4 text-slate-400" />
          </div>
          <div className="mt-2 text-2xl font-bold text-slate-900">{total}</div>
          <div className="mt-1 text-xs text-slate-400">
            已注册 <span className="font-semibold text-green-600">{registered}</span>
            <span className="mx-1 text-slate-300">·</span>
            可用 <span className="font-semibold text-green-600">{available}</span>
          </div>
        </Card>

        <Card className="p-4">
          <div className="flex items-center justify-between text-slate-500 text-xs">
            <span>待注册账号</span>
            <Clock className="h-4 w-4 text-amber-500" />
          </div>
          <div className="mt-2 text-2xl font-bold text-amber-600">{pendingReg}</div>
          <div className="mt-1 text-xs text-slate-400">
            <Link to="/accounts/registrations" className="text-blue-600 hover:underline inline-flex items-center gap-1">
              前往注册队列执行 <ArrowRight className="h-3 w-3" />
            </Link>
          </div>
        </Card>

        <Card className="p-4">
          <div className="flex items-center justify-between text-slate-500 text-xs">
            <span>今日额度耗尽</span>
            <AlertCircle className="h-4 w-4 text-orange-500" />
          </div>
          <div className="mt-2 text-2xl font-bold text-orange-600">{limitReached}</div>
          <div className="mt-1 text-xs text-slate-400">次日自动恢复重置</div>
        </Card>

        <Card className="p-4">
          <div className="flex items-center justify-between text-slate-500 text-xs">
            <span>已禁用账号</span>
            <ShieldCheck className="h-4 w-4 text-slate-400" />
          </div>
          <div className="mt-2 text-2xl font-bold text-slate-700">{disabled}</div>
          <div className="mt-1 text-xs text-slate-400">管理员停用或风控</div>
        </Card>
      </div>

      {/* 账号资源表格 */}
      <Card>
        <div className="border-b border-slate-100 p-4 flex items-center justify-between">
          <h3 className="text-sm font-semibold text-slate-800">账号资源列表</h3>
          <Button variant="ghost" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
        <Table
          headers={["邮箱", "昵称", "状态", "当日用量", "额度", "最近登录", "操作"]}
          empty={!data || data.items.length === 0 ? <EmptyRow colSpan={7} text="暂无账号" /> : undefined}
        >
          {accounts.map((a) => (
            <tr key={a.id}>
              <Td className="font-medium text-slate-800">{a.email}</Td>
              <Td className="text-xs text-slate-500">{a.nickname ?? "-"}</Td>
              <Td>
                <StatusBadge status={a.status} />
              </Td>
              <Td className="text-xs text-slate-500">
                <span className={a.daily_used >= a.daily_limit ? "font-bold text-orange-600" : ""}>
                  {a.daily_used}
                </span>
              </Td>
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
        {data && data.total > 0 && (
          <div className="border-t border-slate-100 p-4 flex items-center justify-between">
            <Button
              variant="secondary"
              size="sm"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
            >
              <ChevronLeft className="h-4 w-4" />
              上一页
            </Button>
            <span className="text-xs text-slate-500">
              第 {page} / {totalPages} 页 · 共 {data.total} 个账号
            </span>
            <Button
              variant="secondary"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
            >
              下一页
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        )}
      </Card>

      {/* 单个新增 Dialog */}
      <Dialog
        open={creating}
        title="新增账号"
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreating(false)}>
              取消
            </Button>
            <Button onClick={create} disabled={submitting}>
              {submitting ? "创建中..." : "确认创建"}
            </Button>
          </>
        }
      >
        <div className="space-y-3">
          {formError && <div className="text-xs text-red-600">{formError}</div>}
          <Input label="邮箱" required value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input
            label="密码 (可选)"
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <Input label="昵称 (可选)" value={nickname} onChange={(e) => setNickname(e.target.value)} />
          <Input
            label="每日额度"
            type="number"
            value={dailyLimit}
            onChange={(e) => setDailyLimit(e.target.value)}
          />
        </div>
      </Dialog>

      {/* 批量导入向导 Dialog */}
      <Dialog
        open={importWizardOpen}
        title="批量导入账号向导"
        onClose={() => setImportWizardOpen(false)}
        footer={
          <>
            {importStep === "select" ? (
              <>
                <Button variant="secondary" onClick={() => setImportWizardOpen(false)}>
                  取消
                </Button>
                <Button onClick={handleUploadAndPreview} disabled={!selectedFile || previewing}>
                  {previewing ? "正在预检..." : "上传并预检"}
                </Button>
              </>
            ) : (
              <>
                <Button variant="secondary" onClick={() => setImportStep("select")} disabled={committing}>
                  返回重选
                </Button>
                <Button onClick={handleCommitImport} disabled={committing}>
                  {committing ? "提交中..." : "确认导入并执行"}
                </Button>
              </>
            )}
          </>
        }
      >
        {importStep === "select" ? (
          <div className="space-y-4 text-sm">
            <div className="space-y-2">
              <label className="block font-medium text-slate-700">1. 下载格式模板</label>
              <div className="flex gap-2">
                <Button variant="secondary" size="sm" onClick={() => handleDownloadTemplate("txt")}>
                  下载 TXT 模板 (email----password)
                </Button>
                <Button variant="secondary" size="sm" onClick={() => handleDownloadTemplate("csv")}>
                  下载 CSV 模板
                </Button>
              </div>
            </div>

            <div className="space-y-2">
              <label className="block font-medium text-slate-700">2. 导入账号状态</label>
              <div className="flex gap-4">
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="radio"
                    name="importMode"
                    value="待注册"
                    checked={importMode === "待注册"}
                    onChange={() => setImportMode("待注册")}
                  />
                  <span>待注册 (导入后自动进入注册队列)</span>
                </label>
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="radio"
                    name="importMode"
                    value="已注册"
                    checked={importMode === "已注册"}
                    onChange={() => setImportMode("已注册")}
                  />
                  <span>已注册 (可直接用于图书下载)</span>
                </label>
              </div>
            </div>

            <div className="space-y-2">
              <label className="block font-medium text-slate-700">3. 选择文件</label>
              <input
                ref={fileInputRef}
                type="file"
                accept=".txt,.csv"
                className="block w-full text-xs text-slate-500 file:mr-4 file:py-2 file:px-4 file:rounded-md file:border-0 file:text-xs file:font-semibold file:bg-blue-50 file:text-blue-700 hover:file:bg-blue-100"
                onChange={(e) => {
                  if (e.target.files && e.target.files.length > 0) {
                    setSelectedFile(e.target.files[0]);
                  }
                }}
              />
            </div>
          </div>
        ) : (
          previewData && (
            <div className="space-y-4 text-xs">
              <div className="rounded-lg bg-slate-50 p-3 border border-slate-200">
                <div className="font-semibold text-slate-800 text-sm mb-1">文件预检结果</div>
                <div className="grid grid-cols-2 gap-2 text-slate-600">
                  <div>总读取行数: <span className="font-bold">{previewData.total_rows}</span></div>
                  <div>有效账号数: <span className="font-bold text-green-600">{previewData.valid_rows}</span></div>
                  <div>文件内重复: <span className="text-amber-600">{previewData.duplicate_in_file}</span></div>
                  <div>总库已存在: <span className="text-amber-600">{previewData.duplicate_in_library}</span></div>
                </div>
              </div>

              {importMode === "待注册" && (
                <div className="space-y-3 rounded-lg border border-blue-100 bg-blue-50/50 p-3">
                  <div className="text-xs font-semibold text-blue-900">
                    待注册账号将强制自动加入内部注册队列
                  </div>
                  <div className="space-y-2">
                    <Input
                      label="分组名称 (可选，默认按时间命名)"
                      value={regBatchName}
                      onChange={(e) => setRegBatchName(e.target.value)}
                    />
                    <Input
                      label="队列优先级"
                      type="number"
                      value={regPriority}
                      onChange={(e) => setRegPriority(e.target.value)}
                    />
                    <label className="flex items-center gap-2 cursor-pointer text-slate-700">
                      <input
                        type="checkbox"
                        checked={startImmediately}
                        onChange={(e) => setStartImmediately(e.target.checked)}
                      />
                      <span>导入后立即启动注册</span>
                    </label>
                  </div>
                </div>
              )}
            </div>
          )
        )}
      </Dialog>

      {/* Outlook 同步向导 Dialog */}
      <Dialog
        open={outlookWizardOpen}
        title={
          outlookStep === "settings"
            ? "同步 Outlook 账号（第 1/2 步：设置云端统一注册密码与队列）"
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
                确认同步并加入注册队列
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
                <span className="text-xs font-semibold text-slate-800">同步后为勾选账号自动加入「注册队列」</span>
              </label>

              {outlookCreateBatch && (
                <div className="space-y-2 pl-6">
                  <Input
                    label="队列批次名称（选填）"
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
                    {outlookPreview.accounts.map((acc: any) => (
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
