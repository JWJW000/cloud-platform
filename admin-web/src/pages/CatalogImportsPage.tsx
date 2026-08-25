import { useEffect, useState } from "react";
import {
  AlertCircle,
  Database,
  Plus,
  RefreshCw,
  UploadCloud,
  ShieldAlert,
} from "lucide-react";
import {
  listCatalogSources,
  listCatalogImportRuns,
  listCatalogQuarantined,
  listCatalogServerManifests,
  previewCatalogImport,
  submitCatalogImport,
  resolveCatalogQuarantine,
} from "../lib/api";
import {
  CatalogSource,
  ImportPreviewResult,
  ImportRun,
  QuarantinedRecord,
} from "../lib/types";
import { Card, Spinner, StatusBadge, Button, Input } from "../components/ui";
import { useToast } from "../context/ToastContext";

export function CatalogImportsPage() {
  const [sources, setSources] = useState<CatalogSource[]>([]);
  const [runs, setRuns] = useState<ImportRun[]>([]);
  const [quarantined, setQuarantined] = useState<QuarantinedRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 导入模态框状态
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
    if (file.size > 8 * 1024 * 1024) {
      toastError("单个补充书单暂限 8 MiB；更大清单请先拆分或登记服务器 manifest");
      setSelectedFile(null);
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
      success("隔离记录已修正并重新入库");
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
        <Card className="p-5 lg:col-span-2">
          <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4">
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
            <div className="overflow-x-auto">
              <table className="w-full text-left text-sm">
                <thead className="border-b border-slate-200 text-xs font-semibold text-slate-500 uppercase">
                  <tr>
                    <th className="pb-3">运行 ID</th>
                    <th className="pb-3">状态</th>
                    <th className="pb-3">总数</th>
                    <th className="pb-3">成功</th>
                    <th className="pb-3">重复跳过</th>
                    <th className="pb-3">隔离行</th>
                    <th className="pb-3">时间</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-slate-100">
                  {runs.map((r) => (
                    <tr key={r.id}>
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
            书名缺失或脏格式数据将被隔离，修复后可重新归并入库。
          </span>
        </div>

        {quarantined.length === 0 ? (
          <div className="text-center py-6 text-sm text-slate-400">
            隔离区为空，所有数据均正常归并入库。
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
                      确认并入库
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

      {/* 导入模态框 */}
      {showModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
          <div className="bg-white rounded-xl max-w-2xl w-full p-6 space-y-4 max-h-[90vh] overflow-y-auto">
            <h3 className="text-lg font-bold text-slate-900">新建数据导入与预检</h3>

            <div className="space-y-3 text-sm">
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
                  <span className="mt-1 text-xs text-slate-400">UTF-8，单文件不超过 8 MiB</span>
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
                {importing ? "正在导入..." : "确认提交入库"}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
