// 下载批次页：列表 + 真实 CSV 文件两阶段导入（预检 -> 提交） + 开始/暂停/恢复/取消。
import { useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can, deniedMessage } from "../lib/permissions";
import {
  cancelBatch,
  commitBooksImport,
  listBatches,
  pauseBatch,
  previewBooksImport,
  resumeBatch,
  startBatch,
} from "../lib/api";
import { ApiError, type Batch, type BookImportPreview } from "../lib/types";
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
import { Download, FileUp, AlertTriangle } from "lucide-react";

export function BatchesPage() {
  const { user } = useAuth();
  const toast = useToast();
  const { data, loading, error, reload } = useApi<Batch[]>(listBatches);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [wizardOpen, setWizardOpen] = useState(false);
  const [step, setStep] = useState<"select" | "preview">("select");
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [batchName, setBatchName] = useState("");
  const [downloadFormat, setDownloadFormat] = useState("pdf");
  const [priority, setPriority] = useState("10");
  const [maxAttempts, setMaxAttempts] = useState("3");

  const [previewing, setPreviewing] = useState(false);
  const [previewData, setPreviewData] = useState<BookImportPreview | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const canManage = can(user?.role, "manage_batch");

  const runAction = async (batch: Batch, action: "start" | "pause" | "resume" | "cancel") => {
    const label = {
      start: "开始",
      pause: "暂停",
      resume: "恢复",
      cancel: "取消",
    }[action];
    try {
      if (action === "start") await startBatch(batch.id);
      else if (action === "pause") await pauseBatch(batch.id);
      else if (action === "resume") await resumeBatch(batch.id);
      else if (action === "cancel") await cancelBatch(batch.id);

      toast.success(`批次「${batch.name}」已${label}`);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : `${label}失败`);
      if (e instanceof ApiError && e.status === 409) reload();
    }
  };

  const handleOpenWizard = () => {
    setStep("select");
    setSelectedFile(null);
    setBatchName("");
    setDownloadFormat("pdf");
    setPriority("10");
    setMaxAttempts("3");
    setPreviewData(null);
    setWizardOpen(true);
  };

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0] ?? null;
    setSelectedFile(file);
    if (file && !batchName) {
      setBatchName(file.name.replace(/\.csv$/i, ""));
    }
  };

  const handleUploadAndPreview = async () => {
    if (!selectedFile) {
      toast.error("请先选择 CSV 文件");
      return;
    }
    setPreviewing(true);
    try {
      const formData = new FormData();
      formData.append("file", selectedFile);
      formData.append("batch_name", batchName.trim());
      formData.append("download_format", downloadFormat);
      formData.append("priority", priority);
      formData.append("max_attempts", maxAttempts);

      const preview = await previewBooksImport(formData);
      setPreviewData(preview);
      setStep("preview");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "文件预检失败");
    } finally {
      setPreviewing(false);
    }
  };

  const handleCommit = async (startImmediately: boolean) => {
    if (!previewData) return;
    setSubmitting(true);
    try {
      const res = await commitBooksImport({
        import_token: previewData.import_token,
        start_immediately: startImmediately,
      });
      toast.success(
        `批次「${res.batch.name}」创建成功（有效 ${previewData.valid_rows} 本，去重 ${res.deduplicated} 本）`
      );
      setWizardOpen(false);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "提交创建批次失败");
    } finally {
      setSubmitting(false);
    }
  };

  const handleDownloadTemplate = () => {
    const csvContent = "书名,作者,出版社,ISBN\n天上有个大薯片,夏忠波著,电子工业出版社有限公司,9787121110627\n计算机网络,谢希仁,电子工业出版社,9787121110000\n";
    const blob = new Blob([new Uint8Array([0xef, 0xbb, 0xbf]), csvContent], {
      type: "text/csv;charset=utf-8;",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "books_template.csv";
    a.click();
    URL.revokeObjectURL(url);
  };

  if (loading) return <Spinner label="正在加载批次..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">下载批次</h2>
          <p className="text-sm text-slate-500">上传图书 CSV 文件创建批次，Master 自动下发给在线 Worker</p>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="secondary" size="sm" onClick={handleDownloadTemplate} title="下载标准 CSV 模板">
            <Download className="mr-1 h-3.5 w-3.5" />
            下载 CSV 模板
          </Button>
          {canManage ? (
            <Button onClick={handleOpenWizard}>
              <FileUp className="mr-1 h-4 w-4" />
              上传 CSV 建批
            </Button>
          ) : (
            <span className="text-sm text-slate-400" title={deniedMessage("manage_batch")}>
              仅管理员可导入
            </span>
          )}
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>

      <Card>
        <Table
          headers={["名称", "状态", "优先级", "格式", "创建时间", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={6} text="暂无批次" /> : undefined}
        >
          {(data ?? []).map((b) => (
            <tr key={b.id}>
              <Td>
                <Link to={`/batches/${b.id}`} className="font-medium text-blue-700 hover:underline">
                  {b.name}
                </Link>
              </Td>
              <Td>
                <StatusBadge status={b.status} />
              </Td>
              <Td>{b.priority}</Td>
              <Td className="text-xs text-slate-500">{b.download_format}</Td>
              <Td className="text-xs text-slate-500">{formatTime(b.created_at)}</Td>
              <Td>
                {canManage ? (
                  <div className="flex items-center gap-1">
                    {b.status === "待开始" && (
                      <Button size="sm" onClick={() => runAction(b, "start")}>
                        开始
                      </Button>
                    )}
                    {b.status === "执行中" && (
                      <>
                        <Button size="sm" variant="secondary" onClick={() => runAction(b, "pause")}>
                          暂停
                        </Button>
                        <Button size="sm" variant="danger" onClick={() => runAction(b, "cancel")}>
                          取消
                        </Button>
                      </>
                    )}
                    {b.status === "已暂停" && (
                      <Button size="sm" variant="success" onClick={() => runAction(b, "resume")}>
                        恢复
                      </Button>
                    )}
                    <Link to={`/batches/${b.id}`}>
                      <Button size="sm" variant="ghost">
                        详情
                      </Button>
                    </Link>
                  </div>
                ) : (
                  <span className="text-xs text-slate-400" title={deniedMessage("manage_batch")}>
                    只读
                  </span>
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>

      {/* CSV 导入向导 Dialog */}
      <Dialog
        open={wizardOpen}
        title={step === "select" ? "上传图书 CSV 建批（第 1/2 步：选择与配置）" : "预检统计与确认（第 2/2 步：核对与提交）"}
        onClose={() => !submitting && setWizardOpen(false)}
        footer={
          step === "select" ? (
            <>
              <Button variant="secondary" onClick={() => setWizardOpen(false)}>
                取消
              </Button>
              <Button loading={previewing} onClick={handleUploadAndPreview}>
                上传并预检
              </Button>
            </>
          ) : (
            <>
              <Button variant="secondary" disabled={submitting} onClick={() => setStep("select")}>
                上一步
              </Button>
              <Button variant="secondary" loading={submitting} onClick={() => handleCommit(false)}>
                仅创建（待开始）
              </Button>
              <Button loading={submitting} onClick={() => handleCommit(true)}>
                创建并立即开始
              </Button>
            </>
          )
        }
      >
        {step === "select" ? (
          <div className="space-y-4">
            <div>
              <label className="mb-1 block text-xs font-medium text-slate-600">
                选择图书 CSV 文件（单列书名 或 四列：书名,作者,出版社,ISBN）
              </label>
              <input
                ref={fileInputRef}
                type="file"
                accept=".csv,text/csv"
                onChange={handleFileChange}
                className="w-full text-sm text-slate-500 file:mr-4 file:rounded-md file:border-0 file:bg-blue-50 file:px-4 file:py-2 file:text-sm file:font-semibold file:text-blue-700 hover:file:bg-blue-100"
              />
              {selectedFile && (
                <p className="mt-1 text-xs text-slate-500">
                  已选择：{selectedFile.name} ({(selectedFile.size / 1024).toFixed(1)} KB)
                </p>
              )}
            </div>

            <Input
              label="批次名称"
              value={batchName}
              onChange={(e) => setBatchName(e.target.value)}
              placeholder="例如：2026年8月采购书单"
            />

            <div className="grid grid-cols-3 gap-3">
              <div>
                <label className="mb-1 block text-xs font-medium text-slate-600">下载格式</label>
                <Select value={downloadFormat} onChange={(e) => setDownloadFormat(e.target.value)}>
                  <option value="pdf">PDF</option>
                  <option value="epub">EPUB</option>
                </Select>
              </div>
              <Input
                label="优先级（越大越优先）"
                type="number"
                value={priority}
                onChange={(e) => setPriority(e.target.value)}
              />
              <Input
                label="最大重试次数"
                type="number"
                value={maxAttempts}
                onChange={(e) => setMaxAttempts(e.target.value)}
              />
            </div>
          </div>
        ) : (
          previewData && (
            <div className="space-y-4">
              {/* 统计指标卡片 */}
              <div className="grid grid-cols-3 gap-2 text-center sm:grid-cols-6">
                <div className="rounded-lg bg-slate-50 p-2 border border-slate-200">
                  <div className="text-xs text-slate-500">总行数</div>
                  <div className="text-lg font-bold text-slate-800">{previewData.total_rows}</div>
                </div>
                <div className="rounded-lg bg-blue-50 p-2 border border-blue-200">
                  <div className="text-xs text-blue-600">有效待下</div>
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
                <div className="rounded-lg bg-green-50 p-2 border border-green-200">
                  <div className="text-xs text-green-600">已入库</div>
                  <div className="text-lg font-bold text-green-700">{previewData.already_ingested}</div>
                </div>
                <div className="rounded-lg bg-red-50 p-2 border border-red-200">
                  <div className="text-xs text-red-600">错误行数</div>
                  <div className="text-lg font-bold text-red-700">{previewData.error_rows}</div>
                </div>
              </div>

              {/* 警告信息 */}
              {previewData.warnings.length > 0 && (
                <div className="rounded-md bg-amber-50 p-3 border border-amber-200 text-xs text-amber-800 space-y-1 max-h-24 overflow-y-auto">
                  <div className="flex items-center font-medium gap-1">
                    <AlertTriangle className="h-3.5 w-3.5" />
                    预检警告提示（{previewData.warnings.length}）：
                  </div>
                  {previewData.warnings.slice(0, 5).map((w, idx) => (
                    <div key={idx}>• {w}</div>
                  ))}
                </div>
              )}

              {/* 预览明细表格 */}
              <div>
                <div className="mb-1 flex items-center justify-between">
                  <span className="text-xs font-medium text-slate-700">解析预览（前 {previewData.preview.length} 行）：</span>
                </div>
                <div className="max-h-60 overflow-y-auto rounded border border-slate-200">
                  <table className="w-full text-left text-xs">
                    <thead className="bg-slate-50 text-slate-600 border-b">
                      <tr>
                        <th className="p-1.5 w-12 text-center">行号</th>
                        <th className="p-1.5">书名</th>
                        <th className="p-1.5">作者</th>
                        <th className="p-1.5">ISBN</th>
                        <th className="p-1.5 w-24">状态</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-slate-100">
                      {previewData.preview.map((p, idx) => (
                        <tr key={idx} className={p.status === "错误" ? "bg-red-50/50" : ""}>
                          <td className="p-1.5 text-center text-slate-400">{p.line}</td>
                          <td className="p-1.5 font-medium text-slate-800 max-w-44 truncate" title={p.title}>
                            {p.title}
                          </td>
                          <td className="p-1.5 text-slate-500 max-w-28 truncate">{p.author ?? "-"}</td>
                          <td className="p-1.5 text-slate-500 font-mono">{p.isbn ?? "-"}</td>
                          <td className="p-1.5">
                            <span
                              className={`inline-block px-1.5 py-0.5 rounded text-[11px] font-medium ${
                                p.status === "有效待下"
                                  ? "bg-blue-100 text-blue-800"
                                  : p.status === "已入库"
                                  ? "bg-green-100 text-green-800"
                                  : p.status === "库内已有"
                                  ? "bg-purple-100 text-purple-800"
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
    </div>
  );
}
