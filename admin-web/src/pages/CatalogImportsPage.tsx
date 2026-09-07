import { useEffect, useState } from "react";
import {
  AlertCircle,
  ChevronLeft,
  ChevronRight,
  Database,
  Download,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  UploadCloud,
  ShieldAlert,
  X,
} from "lucide-react";
import {
  catalogImportRunExportUrl,
  listCatalogSources,
  listCatalogImportRunItems,
  listCatalogImportRuns,
  listCatalogQuarantined,
  listCatalogServerManifests,
  previewCatalogImport,
  submitCatalogImport,
  resolveCatalogQuarantine,
  retryCatalogAcquisition,
} from "../lib/api";
import {
  CatalogSource,
  ImportPreviewResult,
  ImportRun,
  ImportRunItem,
  ImportRunItemsPage,
  ImportRunOutcome,
  ImportRunOutcomeCounts,
  QuarantinedRecord,
} from "../lib/types";
import { Card, Spinner, StatusBadge, Button, Input } from "../components/ui";
import { GlobalDownloadControlCard } from "../components/GlobalDownloadControlCard";
import { useToast } from "../context/ToastContext";

const OUTCOME_META: Record<ImportRunOutcome, { label: string; className: string }> = {
  downloaded: { label: "本批下载成功", className: "bg-emerald-50 text-emerald-700 border-emerald-200" },
  already_owned: { label: "总库已有", className: "bg-cyan-50 text-cyan-700 border-cyan-200" },
  pending: { label: "等待下载", className: "bg-slate-100 text-slate-700 border-slate-200" },
  running: { label: "正在下载", className: "bg-blue-50 text-blue-700 border-blue-200" },
  retryable: { label: "可重试", className: "bg-amber-50 text-amber-700 border-amber-200" },
  site_not_found: { label: "站点未收录", className: "bg-violet-50 text-violet-700 border-violet-200" },
  failed: { label: "下载失败", className: "bg-rose-50 text-rose-700 border-rose-200" },
  needs_review: { label: "需人工确认", className: "bg-orange-50 text-orange-700 border-orange-200" },
  quarantined: { label: "导入隔离", className: "bg-zinc-100 text-zinc-700 border-zinc-200" },
};

function OutcomeBadge({ outcome }: { outcome: ImportRunOutcome }) {
  const meta = OUTCOME_META[outcome];
  return <span className={`inline-flex rounded-full border px-2 py-0.5 text-xs font-medium ${meta.className}`}>{meta.label}</span>;
}

export function CatalogImportsPage() {
  const [sources, setSources] = useState<CatalogSource[]>([]);
  const [runs, setRuns] = useState<ImportRun[]>([]);
  const [quarantined, setQuarantined] = useState<QuarantinedRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 导入批次下载明细
  const [selectedRun, setSelectedRun] = useState<ImportRun | null>(null);
  const [runItems, setRunItems] = useState<ImportRunItemsPage | null>(null);
  const [runSummary, setRunSummary] = useState<ImportRunOutcomeCounts | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [outcomeFilter, setOutcomeFilter] = useState<ImportRunOutcome | "">("");
  const [detailQuery, setDetailQuery] = useState("");
  const [appliedDetailQuery, setAppliedDetailQuery] = useState("");
  const [detailCursor, setDetailCursor] = useState("");
  const [cursorHistory, setCursorHistory] = useState<string[]>([]);

  // 导入模态框状态
  const [importMode, setImportMode] = useState<"owned" | "download">("download");
  const [showModal, setShowModal] = useState(false);
  const [sourceName, setSourceName] = useState("");
  const [fileName, setFileName] = useState("");
  const [textContent, setTextContent] = useState("");
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [serverManifests, setServerManifests] = useState<Array<{ id: string; size_bytes: number }>>([]);
  const [serverManifest, setServerManifest] = useState("");
  const [preview, setPreview] = useState<ImportPreviewResult | null>(null);
  const [importing, setImporting] = useState(false);

  // 解决隔离记录状态
  const [resolvingId, setResolvingId] = useState<string | null>(null);
  const [resolveTitle, setResolveTitle] = useState("");

  const { success, error: toastError } = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [srcs, rns, quar, manifests] = await Promise.all([
        listCatalogSources(),
        listCatalogImportRuns(),
        listCatalogQuarantined(),
        listCatalogServerManifests().catch(() => []),
      ]);
      setSources(srcs);
      setRuns(rns);
      setQuarantined(quar);
      setServerManifests(manifests);
      if (srcs.length > 0 && !sourceName) {
        setSourceName(srcs[0].name);
      }
    } catch (err: any) {
      setError(err.message || "加载导入数据失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const loadRunItems = async () => {
    if (!selectedRun) return;
    try {
      setDetailLoading(true);
      setDetailError(null);
      const data = await listCatalogImportRunItems(selectedRun.id, {
        outcome: outcomeFilter || undefined,
        query: appliedDetailQuery || undefined,
        cursor: detailCursor || undefined,
        limit: 50,
        include_summary: runSummary === null,
      });
      setRunItems(data);
      if (data.summary) setRunSummary(data.summary);
    } catch (err: any) {
      setDetailError(err.message || "加载导入下载明细失败");
    } finally {
      setDetailLoading(false);
    }
  };

  useEffect(() => {
    loadRunItems();
  }, [selectedRun?.id, outcomeFilter, appliedDetailQuery, detailCursor]);

  const openRunDetail = (run: ImportRun) => {
    setSelectedRun(run);
    setRunItems(null);
    setRunSummary(null);
    setOutcomeFilter("");
    setDetailQuery("");
    setAppliedDetailQuery("");
    setDetailCursor("");
    setCursorHistory([]);
  };

  const retryItem = async (item: ImportRunItem) => {
    if (!item.target_id || !item.retryable) return;
    try {
      await retryCatalogAcquisition(item.target_id);
      success(`《${item.title}》已重新加入下载队列`);
      loadRunItems();
    } catch (err: any) {
      toastError(err.message || "重新下载失败");
    }
  };

  const handlePreview = async () => {
    if (!sourceName.trim() || (!serverManifest && (!fileName.trim() || !textContent.trim()))) {
      toastError("请选择补充书单、服务器 manifest，或填写少量录入内容");
      return;
    }
    try {
      setImporting(true);
      const res = await previewCatalogImport({
        source_name: sourceName,
        file_name: fileName,
        text_content: serverManifest ? undefined : textContent,
        server_manifest: serverManifest || undefined,
      });
      setPreview(res);
      success(`预检成功，识别出 ${res.total_rows} 行数据`);
    } catch (err: any) {
      toastError(err.message || "预检失败");
    } finally {
      setImporting(false);
    }
  };

  const handleFileSelection = async (file: File | null) => {
    setSelectedFile(file);
    setServerManifest("");
    setPreview(null);
    if (!file) {
      setTextContent("");
      setFileName("");
      return;
    }
    try {
      const text = await file.text();
      setFileName(file.name);
      setTextContent(text);
    } catch {
      toastError("无法读取所选文件，请使用 UTF-8 CSV/TSV/TXT 文件");
      setSelectedFile(null);
    }
  };

  const handleSubmitImport = async () => {
    if (!sourceName.trim() || (!serverManifest && (!fileName.trim() || !textContent.trim()))) return;
    try {
      setImporting(true);
      const res = await submitCatalogImport({
        import_mode: importMode,
        source_name: sourceName,
        file_name: fileName,
        text_content: serverManifest ? undefined : textContent,
        server_manifest: serverManifest || undefined,
      });
      success(
        `导入完成：成功 ${res.imported_count} 行，重复 ${res.duplicate_count} 行，隔离 ${res.quarantined_count} 行`
      );
      setShowModal(false);
      setTextContent("");
      setSelectedFile(null);
      setServerManifest("");
      setPreview(null);
      loadData();
    } catch (err: any) {
      toastError(err.message || "导入执行失败");
    } finally {
      setImporting(false);
    }
  };

  const handleResolveQuarantine = async (id: string) => {
    if (!resolveTitle.trim()) {
      toastError("请填写修正后的书名");
      return;
    }
    try {
      await resolveCatalogQuarantine(id, { corrected_title: resolveTitle.trim() });
      success("隔离记录已修正并重新加入总库");
      setResolvingId(null);
      setResolveTitle("");
      loadData();
    } catch (err: any) {
      toastError(err.message || "修正失败");
    }
  };

  if (loading && sources.length === 0) {
    return <Spinner label="正在读取导入中心数据..." />;
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-slate-900">总库导入中心</h1>
          <p className="text-xs text-slate-500">
            多格式流式解析、结构自动识别、检查点断点续传与隔离区异常行治理。
          </p>
        </div>
        <Button variant="primary" onClick={() => setShowModal(true)}>
          <Plus className="h-4 w-4 mr-1" />
          新建数据导入
        </Button>
      </div>

      <GlobalDownloadControlCard />

      {error && (
        <div className="rounded-lg bg-red-50 p-4 border border-red-200 text-sm text-red-700 flex items-center gap-2">
          <AlertCircle className="h-5 w-5" />
          {error}
        </div>
      )}

      {/* 数据源与最近运行 */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* 数据源卡片 */}
        <Card className="p-5">
          <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4">
            <div className="flex items-center gap-2">
              <Database className="h-5 w-5 text-blue-600" />
              <h3 className="font-semibold text-slate-900">数据源 ({sources.length})</h3>
            </div>
          </div>
          <div className="space-y-2">
            {sources.map((src) => (
              <div key={src.id} className="p-3 bg-slate-50 rounded-lg border border-slate-200 text-xs flex items-center justify-between">
                <div>
                  <div className="font-bold text-slate-800">{src.name}</div>
                  <div className="text-slate-400 mt-0.5">{src.source_type} / 优先级 {src.priority}</div>
                </div>
                <StatusBadge status="启用" />
              </div>
            ))}
          </div>
        </Card>

        {/* 导入运行历史 */}
        <Card className="p-5 lg:col-span-2 flex flex-col min-h-0">
          <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4 shrink-0">
            <div className="flex items-center gap-2">
              <UploadCloud className="h-5 w-5 text-blue-600" />
              <h3 className="font-semibold text-slate-900">导入运行历史</h3>
            </div>
            <Button variant="ghost" size="sm" onClick={loadData}>
              <RefreshCw className="h-3.5 w-3.5 mr-1" /> 刷新
            </Button>
          </div>

          {runs.length === 0 ? (
            <div className="text-center py-8 text-sm text-slate-400">
              暂无导入运行记录。
            </div>
          ) : (
            <div className="overflow-x-auto max-h-72 overflow-y-auto">
              <table className="w-full text-left text-sm">
                <thead className="sticky top-0 bg-white border-b border-slate-200 text-xs font-semibold text-slate-500 uppercase">
                  <tr>
                    <th className="pb-3">运行 ID</th>
                    <th className="pb-3">状态</th>
                    <th className="pb-3">总数</th>
                    <th className="pb-3">成功导入</th>
                    <th className="pb-3">重复跳过</th>
                    <th className="pb-3">隔离行</th>
                    <th className="pb-3">时间</th>
                    <th className="pb-3 text-right">下载结果</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-slate-100">
                  {runs.map((r) => (
                    <tr key={r.id} className="hover:bg-slate-50/70">
                      <td className="py-2.5 font-mono text-xs text-slate-700">{r.id.slice(0, 8)}</td>
                      <td className="py-2.5">
                        <span className={`px-2 py-0.5 rounded text-xs font-medium ${
                          r.status === "已完成" ? "bg-emerald-50 text-emerald-700" : "bg-blue-50 text-blue-700"
                        }`}>
                          {r.status}
                        </span>
                      </td>
                      <td className="py-2.5 text-slate-700">{r.total_rows}</td>
                      <td className="py-2.5 text-emerald-600 font-bold">{r.imported_count}</td>
                      <td className="py-2.5 text-slate-500">{r.duplicate_count}</td>
                      <td className="py-2.5 text-rose-600 font-bold">{r.quarantined_count}</td>
                      <td className="py-2.5 text-xs text-slate-400">{new Date(r.created_at).toLocaleTimeString()}</td>
                      <td className="py-2.5 text-right">
                        <button
                          type="button"
                          onClick={() => openRunDetail(r)}
                          className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs font-semibold text-blue-700 hover:bg-blue-50 focus:outline-none focus:ring-2 focus:ring-blue-500"
                        >
                          查看明细 <ChevronRight className="h-3.5 w-3.5" />
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      </div>

      {/* 隔离区异常行治理 */}
      <Card className="p-5">
        <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4">
          <div className="flex items-center gap-2">
            <ShieldAlert className="h-5 w-5 text-rose-600" />
            <h3 className="font-semibold text-slate-900">隔离区异常行明细与人工修复</h3>
          </div>
          <span className="text-xs text-slate-400">
            书名缺失或脏格式数据将被隔离，修复后可重新归并到书目总库。
          </span>
        </div>

        {quarantined.length === 0 ? (
          <div className="text-center py-6 text-sm text-slate-400">
            隔离区为空，所有数据均正常归并到书目总库。
          </div>
        ) : (
          <div className="space-y-3">
            {quarantined.map((q) => (
              <div key={q.id} className="p-4 bg-rose-50/20 border border-rose-200 rounded-lg text-xs space-y-2">
                <div className="flex items-center justify-between">
                  <span className="font-mono text-slate-600">
                    行号：第 {q.row_number} 行 / 原因：<strong className="text-rose-600">{q.error_reason}</strong>
                  </span>
                  <span className="text-slate-400">{new Date(q.created_at).toLocaleString()}</span>
                </div>

                <div className="p-2 bg-slate-900 text-slate-100 rounded font-mono overflow-x-auto text-[11px]">
                  {JSON.stringify(q.raw_content)}
                </div>

                {resolvingId === q.id ? (
                  <div className="flex gap-2 pt-2">
                    <Input
                      value={resolveTitle}
                      onChange={(e) => setResolveTitle(e.target.value)}
                      placeholder="输入补齐后的书名..."
                      className="text-xs"
                    />
                    <Button size="sm" variant="primary" onClick={() => handleResolveQuarantine(q.id)}>
                      确认并加入总库
                    </Button>
                    <Button size="sm" variant="secondary" onClick={() => setResolvingId(null)}>
                      取消
                    </Button>
                  </div>
                ) : (
                  <div className="pt-1 flex justify-end">
                    <Button size="sm" variant="secondary" onClick={() => { setResolvingId(q.id); setResolveTitle(""); }}>
                      人工补齐修复
                    </Button>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </Card>

      {/* 导入批次下载结果明细 */}
      {selectedRun && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/55 p-3 sm:p-6" role="dialog" aria-modal="true" aria-label="导入批次下载结果明细">
          <div className="flex h-[92vh] w-full max-w-[1500px] flex-col overflow-hidden rounded-2xl bg-white shadow-2xl">
            <div className="flex shrink-0 items-start justify-between border-b border-slate-200 px-5 py-4">
              <div>
                <div className="flex items-center gap-2">
                  <h2 className="text-lg font-bold text-slate-900">导入与下载结果账本</h2>
                  <span className="rounded bg-slate-100 px-2 py-0.5 font-mono text-xs text-slate-600">#{selectedRun.id.slice(0, 8)}</span>
                </div>
                <p className="mt-1 text-xs text-slate-500">
                  “成功导入”只表示书目已解析；下方独立展示实际下载、可重试失败和站点未收录结果。
                </p>
              </div>
              <button
                type="button"
                onClick={() => setSelectedRun(null)}
                className="rounded-lg p-2 text-slate-400 hover:bg-slate-100 hover:text-slate-700 focus:outline-none focus:ring-2 focus:ring-blue-500"
                aria-label="关闭明细"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {runSummary && (
              <div className="grid shrink-0 grid-cols-2 gap-px border-b border-slate-200 bg-slate-200 sm:grid-cols-5 lg:grid-cols-10">
                {([
                  ["全部", "", runSummary.total],
                  ["本批下载成功", "downloaded", runSummary.downloaded],
                  ["总库已有", "already_owned", runSummary.already_owned],
                  ["等待下载", "pending", runSummary.pending],
                  ["正在下载", "running", runSummary.running],
                  ["可重试", "retryable", runSummary.retryable],
                  ["站点未收录", "site_not_found", runSummary.site_not_found],
                  ["下载失败", "failed", runSummary.failed],
                  ["需人工确认", "needs_review", runSummary.needs_review],
                  ["导入隔离", "quarantined", runSummary.quarantined],
                ] as Array<[string, ImportRunOutcome | "", number]>).map(([label, value, count]) => (
                  <button
                    key={label}
                    type="button"
                    onClick={() => {
                      setOutcomeFilter(value);
                      setDetailCursor("");
                      setCursorHistory([]);
                    }}
                    className={`bg-white px-3 py-3 text-left transition hover:bg-slate-50 focus:outline-none focus:ring-2 focus:ring-inset focus:ring-blue-500 ${outcomeFilter === value ? "shadow-[inset_0_-3px_0_#2563eb]" : ""}`}
                  >
                    <div className="text-[11px] text-slate-500">{label}</div>
                    <div className="mt-0.5 text-lg font-bold tabular-nums text-slate-900">{count.toLocaleString()}</div>
                  </button>
                ))}
              </div>
            )}

            <div className="flex shrink-0 flex-col gap-3 border-b border-slate-200 bg-slate-50 px-5 py-3 sm:flex-row sm:items-center">
              <form
                className="flex min-w-0 flex-1 gap-2"
                onSubmit={(event) => {
                  event.preventDefault();
                  setAppliedDetailQuery(detailQuery.trim());
                  setDetailCursor("");
                  setCursorHistory([]);
                }}
              >
                <div className="relative min-w-0 flex-1 sm:max-w-lg">
                  <Search className="absolute left-3 top-2.5 h-4 w-4 text-slate-400" />
                  <Input
                    value={detailQuery}
                    onChange={(event) => setDetailQuery(event.target.value)}
                    className="pl-9"
                    placeholder="搜索书名、作者、出版社、ISBN 或失败原因"
                  />
                </div>
                <Button type="submit" variant="secondary" size="sm">筛选</Button>
                {(appliedDetailQuery || outcomeFilter) && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setDetailQuery("");
                      setAppliedDetailQuery("");
                      setOutcomeFilter("");
                      setDetailCursor("");
                      setCursorHistory([]);
                    }}
                  >
                    重置
                  </Button>
                )}
              </form>
              <a
                href={catalogImportRunExportUrl(selectedRun.id, {
                  outcome: outcomeFilter || undefined,
                  query: appliedDetailQuery || undefined,
                })}
                className="inline-flex h-9 shrink-0 items-center justify-center gap-1.5 rounded-md bg-emerald-600 px-3 text-sm font-semibold text-white shadow-sm transition hover:bg-emerald-700 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:ring-offset-2"
              >
                <Download className="h-4 w-4" /> 导出当前明细 Excel
              </a>
            </div>

            <div className="min-h-0 flex-1 overflow-auto">
              {detailLoading && !runItems ? (
                <div className="flex h-full items-center justify-center"><Spinner label="正在汇总导入与下载结果..." /></div>
              ) : detailError ? (
                <div className="m-5 rounded-lg border border-red-200 bg-red-50 p-4 text-sm text-red-700">
                  <div className="flex items-center gap-2"><AlertCircle className="h-4 w-4" />{detailError}</div>
                  <Button variant="secondary" size="sm" className="mt-3" onClick={loadRunItems}>重新加载</Button>
                </div>
              ) : runItems?.items.length === 0 ? (
                <div className="flex h-full flex-col items-center justify-center gap-2 text-sm text-slate-500">
                  <Database className="h-8 w-8 text-slate-300" />当前筛选条件下没有明细
                </div>
              ) : (
                <table className="w-full min-w-[1320px] text-left text-sm">
                  <thead className="sticky top-0 z-10 border-b border-slate-200 bg-white/95 text-xs font-semibold text-slate-500 shadow-sm backdrop-blur">
                    <tr>
                      <th className="px-4 py-3">行号</th>
                      <th className="px-4 py-3">书目</th>
                      <th className="px-4 py-3">结果</th>
                      <th className="px-4 py-3">下载状态</th>
                      <th className="px-4 py-3">尝试</th>
                      <th className="px-4 py-3">未成功原因 / 执行信息</th>
                      <th className="px-4 py-3">文件位置</th>
                      <th className="px-4 py-3 text-right">操作</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-slate-100">
                    {runItems?.items.map((item) => (
                      <tr key={item.item_id} className="align-top hover:bg-slate-50/70">
                        <td className="px-4 py-3 font-mono text-xs text-slate-500">{item.row_number}</td>
                        <td className="max-w-80 px-4 py-3">
                          <div className="font-semibold text-slate-900" title={item.title}>{item.title}</div>
                          <div className="mt-1 text-xs text-slate-500">{[item.author, item.publisher].filter(Boolean).join(" · ") || "作者/出版社未提供"}</div>
                          {item.isbn && <div className="mt-0.5 font-mono text-[11px] text-slate-400">ISBN {item.isbn}</div>}
                        </td>
                        <td className="px-4 py-3"><OutcomeBadge outcome={item.outcome} /></td>
                        <td className="px-4 py-3 text-xs text-slate-600">
                          <div>{item.acquisition_status || (item.outcome === "already_owned" ? "无需下载" : "未建立下载目标")}</div>
                          {item.worker_name && <div className="mt-1 text-slate-400">Worker：{item.worker_name}</div>}
                        </td>
                        <td className="px-4 py-3 text-xs tabular-nums text-slate-600">
                          {item.max_attempts > 0 ? `${item.attempts}/${item.max_attempts}` : "-"}
                          {item.next_attempt_at && item.retryable && <div className="mt-1 whitespace-nowrap text-[11px] text-amber-700">{new Date(item.next_attempt_at).toLocaleString()}</div>}
                        </td>
                        <td className="max-w-md px-4 py-3 text-xs">
                          <div className={item.failure_reason ? "text-rose-700" : "text-slate-400"}>{item.failure_reason || "-"}</div>
                          {(item.execution_result || item.execution_stage || item.error_code) && (
                            <div className="mt-1 text-[11px] text-slate-400">
                              {[item.execution_result, item.execution_stage, item.error_code].filter(Boolean).join(" / ")}
                            </div>
                          )}
                        </td>
                        <td className="max-w-64 px-4 py-3 font-mono text-[11px] text-slate-500" title={item.nas_object_key || ""}>{item.nas_object_key || "-"}</td>
                        <td className="px-4 py-3 text-right">
                          {item.retryable && item.target_id ? (
                            <Button size="sm" variant="secondary" onClick={() => retryItem(item)}>
                              <RotateCcw className="mr-1 h-3.5 w-3.5" />重新下载
                            </Button>
                          ) : item.edition_id ? (
                            <a href={`/library/editions/${item.edition_id}`} className="text-xs font-semibold text-blue-600 hover:underline">查看书目</a>
                          ) : <span className="text-xs text-slate-300">-</span>}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div className="flex shrink-0 items-center justify-between border-t border-slate-200 bg-white px-5 py-3">
              <span className="text-xs text-slate-500">
                每页最多 50 条{detailLoading ? " · 正在刷新" : ""}
              </span>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={cursorHistory.length === 0 || detailLoading}
                  onClick={() => {
                    const previous = cursorHistory[cursorHistory.length - 1] ?? "";
                    setCursorHistory((history) => history.slice(0, -1));
                    setDetailCursor(previous);
                  }}
                >
                  <ChevronLeft className="mr-1 h-4 w-4" />上一页
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={!runItems?.next_cursor || detailLoading}
                  onClick={() => {
                    if (!runItems?.next_cursor) return;
                    setCursorHistory((history) => [...history, detailCursor]);
                    setDetailCursor(runItems.next_cursor!);
                  }}
                >
                  下一页<ChevronRight className="ml-1 h-4 w-4" />
                </Button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* 导入模态框 */}
      {showModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
          <div className="bg-white rounded-xl max-w-2xl w-full p-6 space-y-4 max-h-[90vh] overflow-y-auto">
            <h3 className="text-lg font-bold text-slate-900">新建数据导入与预检</h3>

            <div className="space-y-3 text-sm">
              <label className="block font-semibold">导入用途
                <select aria-label="导入用途" value={importMode} onChange={(e) => setImportMode(e.target.value as "owned" | "download")} className="mt-1 w-full rounded-md border border-slate-300 px-3 py-2">
                  <option value="download">待下载书单：缺少有效文件的书自动排队</option>
                  <option value="owned">已拥有书目：仅登记总库，不创建下载任务</option>
                </select>
              </label>
              <div>
                <label className="block text-xs font-semibold text-slate-700 mb-1">数据源名称</label>
                <Input value={sourceName} onChange={(e) => setSourceName(e.target.value)} placeholder="如 cn, en, 图书书目1, 补充书单..." />
              </div>

              <div>
                <label className="block text-xs font-semibold text-slate-700 mb-1">已登记服务器目录 manifest</label>
                <select
                  value={serverManifest}
                  onChange={(event) => {
                    const value = event.target.value;
                    setServerManifest(value);
                    if (value) {
                      setSelectedFile(null);
                      setTextContent("");
                      setFileName(value);
                    }
                  }}
                  className="w-full rounded-md border border-slate-300 bg-white px-3 py-2 text-sm"
                >
                  <option value="">不使用服务器 manifest</option>
                  {serverManifests.map((manifest) => (
                    <option key={manifest.id} value={manifest.id}>
                      {manifest.id} · {(manifest.size_bytes / 1024).toFixed(1)} KiB
                    </option>
                  ))}
                </select>
                <p className="mt-1 text-[11px] text-slate-500">
                  运维通过 DRISSION_CATALOG_MANIFEST_ROOT 登记；这里只显示安全文件名，不接受任意服务器路径。
                </p>
              </div>

              <div>
                <label className="block text-xs font-semibold text-slate-700 mb-1">上传补充书单（主流程）</label>
                <label className="flex cursor-pointer flex-col items-center justify-center rounded-lg border-2 border-dashed border-slate-300 bg-slate-50 px-4 py-7 text-center hover:border-blue-400 hover:bg-blue-50/40">
                  <UploadCloud className="mb-2 h-7 w-7 text-blue-500" />
                  <span className="text-sm font-medium text-slate-700">
                    {selectedFile ? selectedFile.name : "选择 CSV、TSV 或 TXT 文件"}
                  </span>
                  <span className="mt-1 text-xs text-slate-400">支持 CSV、TSV、TXT 文本书单</span>
                  <input
                    type="file"
                    accept=".csv,.tsv,.txt,text/csv,text/tab-separated-values,text/plain"
                    className="sr-only"
                    disabled={!!serverManifest}
                    onChange={(event) => handleFileSelection(event.target.files?.[0] ?? null)}
                  />
                </label>
              </div>

              <details className="rounded-lg border border-slate-200 p-3">
                <summary className="cursor-pointer text-xs font-medium text-slate-600">
                  少量临时录入（次要方式，最多 200 行）
                </summary>
                <div className="mt-3 space-y-2">
                  <Input
                    value={fileName}
                    onChange={(e) => setFileName(e.target.value)}
                    placeholder="临时录入名称，如 supplement.csv"
                    disabled={!!selectedFile || !!serverManifest}
                  />
                  <textarea
                    rows={5}
                    value={selectedFile || serverManifest ? "" : textContent}
                    onChange={(e) => {
                      const lines = e.target.value.split(/\r?\n/);
                      if (lines.length <= 200) setTextContent(e.target.value);
                    }}
                    disabled={!!selectedFile || !!serverManifest}
                    placeholder="title,author,publisher,isbn,format"
                    className="w-full rounded-md border border-slate-300 p-2 font-mono text-xs disabled:bg-slate-100"
                  />
                </div>
              </details>

              {preview && (
                <div className="p-3 bg-blue-50 border border-blue-200 rounded-lg text-xs space-y-1.5">
                  <div className="font-bold text-blue-800">
                    预检报告：已识别 {preview.total_rows} 行数据
                  </div>
                  <div className="text-slate-600">文件哈希：{preview.file_sha256.slice(0, 16)}...</div>
                  {preview.sample_rows.length > 0 && (
                    <div className="text-slate-700">
                      首行样本：{preview.sample_rows[0].title} / {preview.sample_rows[0].author || "无作者"}
                    </div>
                  )}
                </div>
              )}
            </div>

            <div className="flex items-center justify-end gap-3 pt-3 border-t border-slate-100">
              <Button variant="secondary" onClick={() => setShowModal(false)}>
                取消
              </Button>
              <Button variant="secondary" onClick={handlePreview} disabled={importing}>
                预检解析
              </Button>
              <Button variant="primary" onClick={handleSubmitImport} disabled={importing}>
                {importing ? "正在导入..." : (importMode === "download" ? "确认导入并排队下载" : "确认加入已拥有书目")}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
