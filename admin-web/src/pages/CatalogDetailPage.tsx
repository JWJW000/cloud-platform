import { useEffect, useState } from "react";
import { useParams, Link } from "react-router-dom";
import {
  ArrowLeft,
  BookOpen,
  CheckCircle2,
  Clock,
  Database,
  FileCheck,
  HardDrive,
  RefreshCw,
} from "lucide-react";
import { getCatalogEdition, retryCatalogAcquisition } from "../lib/api";
import { EditionDetail } from "../lib/types";
import { Card, Spinner, StatusBadge, Button } from "../components/ui";
import { useToast } from "../context/ToastContext";

export function CatalogDetailPage() {
  const { id } = useParams<{ id: string }>();
  const [detail, setDetail] = useState<EditionDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<"provenance" | "assets" | "holdings" | "timeline">("provenance");
  const { success, error: toastError } = useToast();

  const loadData = async () => {
    if (!id) return;
    try {
      setLoading(true);
      setError(null);
      const res = await getCatalogEdition(id);
      setDetail(res);
    } catch (err: any) {
      setError(err.message || "加载图书详情失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, [id]);

  const handleRetry = async () => {
    if (!detail?.acquisition_target) return;
    try {
      await retryCatalogAcquisition(detail.acquisition_target.id);
      success("已触发立即重新获取");
      loadData();
    } catch (err: any) {
      toastError(err.message || "重试失败");
    }
  };

  if (loading) {
    return <Spinner label="正在读取图书全景详情与溯源证据..." />;
  }

  if (error || !detail) {
    return (
      <div className="space-y-4">
        <Link to="/catalog/search" className="inline-flex items-center gap-1 text-sm text-blue-600 hover:text-blue-800">
          <ArrowLeft className="h-4 w-4" /> 返回检索
        </Link>
        <Card className="p-8 text-center text-red-600">
          {error || "图书版本不存在"}
        </Card>
      </div>
    );
  }

  const { edition, work, identifiers, contributors, source_records, source_assets, holdings, acquisition_target, executions } = detail;

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <Link
          to="/catalog/search"
          className="inline-flex items-center gap-1 text-sm text-slate-500 hover:text-slate-800 font-medium"
        >
          <ArrowLeft className="h-4 w-4" /> 返回检索列表
        </Link>
        <div className="flex items-center gap-2">
          {acquisition_target && acquisition_target.status !== "已下载" && (
            <Button variant="secondary" size="sm" onClick={handleRetry}>
              <RefreshCw className="h-3.5 w-3.5 mr-1" /> 重新入队调度
            </Button>
          )}
        </div>
      </div>

      {/* 头部信息卡片 */}
      <Card className="p-6">
        <div className="flex flex-col md:flex-row md:items-start justify-between gap-6">
          <div className="space-y-3 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-xs px-2 py-0.5 rounded bg-blue-100 text-blue-800 font-semibold">
                {work.work_type}
              </span>
              <span className="text-xs px-2 py-0.5 rounded bg-slate-100 text-slate-700 font-medium uppercase">
                {edition.language}
              </span>
              <StatusBadge status={work.resolution_status} />
            </div>

            <h1 className="text-2xl font-bold text-slate-900 leading-tight">
              {edition.edition_title}
            </h1>

            <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 gap-y-2 gap-x-6 text-sm text-slate-600">
              <div>
                <span className="text-slate-400">责任者 / 作者：</span>
                <span className="font-medium text-slate-800">
                  {contributors.length > 0 ? contributors.map((c) => c.name).join(", ") : "未提供"}
                </span>
              </div>
              <div>
                <span className="text-slate-400">出版者：</span>
                {edition.publisher_id ? (
                  <Link
                    to={`/publishers/${edition.publisher_id}`}
                    className="font-semibold text-blue-600 hover:underline"
                  >
                    {edition.publisher}
                  </Link>
                ) : (
                  <span className="font-medium text-slate-800">{edition.publisher || "未提供"}</span>
                )}
              </div>
              <div>
                <span className="text-slate-400">出版年份/日期：</span>
                <span className="font-medium text-slate-800">
                  {edition.publish_year || edition.publish_date_text || "未知"}
                </span>
              </div>
            </div>

            {/* 标识符列表 */}
            {identifiers.length > 0 && (
              <div className="flex flex-wrap items-center gap-2 pt-2 text-xs">
                <span className="text-slate-400">标识符：</span>
                {identifiers.map((ident) => (
                  <span key={ident.id} className="font-mono bg-slate-100 text-slate-800 px-2 py-1 rounded border border-slate-200">
                    <strong>{ident.identifier_type.toUpperCase()}:</strong> {ident.normalized_value}
                  </span>
                ))}
              </div>
            )}
          </div>

          {/* 状态徽章区 */}
          <div className="p-4 bg-slate-50 border border-slate-200 rounded-xl flex flex-col items-center justify-center min-w-[200px] text-center shrink-0">
            <div className="text-xs text-slate-500 mb-1">全局获取状态</div>
            <div className="text-lg font-bold text-slate-900">
              {acquisition_target ? acquisition_target.status : "未入获取池"}
            </div>
            {holdings.length > 0 ? (
              <div className="mt-2 flex items-center gap-1 text-xs text-emerald-600 font-semibold">
                <CheckCircle2 className="h-4 w-4" /> 当前有可用文件 ({holdings.length})
              </div>
            ) : (
              <div className="mt-2 text-xs text-amber-600">尚未入库有效文件</div>
            )}
          </div>
        </div>
      </Card>

      {/* Tab 选项卡 */}
      <div className="border-b border-slate-200 flex gap-6 text-sm font-medium">
        <button
          onClick={() => setActiveTab("provenance")}
          className={`pb-3 transition border-b-2 flex items-center gap-1.5 ${
            activeTab === "provenance"
              ? "border-blue-600 text-blue-600 font-bold"
              : "border-transparent text-slate-500 hover:text-slate-700"
          }`}
        >
          <Database className="h-4 w-4" /> 原始来源与出处溯源 ({source_records.length})
        </button>

        <button
          onClick={() => setActiveTab("assets")}
          className={`pb-3 transition border-b-2 flex items-center gap-1.5 ${
            activeTab === "assets"
              ? "border-blue-600 text-blue-600 font-bold"
              : "border-transparent text-slate-500 hover:text-slate-700"
          }`}
        >
          <BookOpen className="h-4 w-4" /> 来源候选文件 ({source_assets.length})
        </button>

        <button
          onClick={() => setActiveTab("holdings")}
          className={`pb-3 transition border-b-2 flex items-center gap-1.5 ${
            activeTab === "holdings"
              ? "border-blue-600 text-blue-600 font-bold"
              : "border-transparent text-slate-500 hover:text-slate-700"
          }`}
        >
          <HardDrive className="h-4 w-4" /> 文件资产与哈希校验 ({holdings.length})
        </button>

        <button
          onClick={() => setActiveTab("timeline")}
          className={`pb-3 transition border-b-2 flex items-center gap-1.5 ${
            activeTab === "timeline"
              ? "border-blue-600 text-blue-600 font-bold"
              : "border-transparent text-slate-500 hover:text-slate-700"
          }`}
        >
          <Clock className="h-4 w-4" /> 获取执行时间线 ({executions.length})
        </button>
      </div>

      {/* Tab 1: 原始出处溯源 */}
      {activeTab === "provenance" && (
        <div className="space-y-4">
          {source_records.map((sr) => (
            <Card key={sr.id} className="p-5 space-y-3">
              <div className="flex items-center justify-between border-b border-slate-100 pb-2">
                <div className="text-xs font-mono text-slate-500">
                  来源行唯一定位：<strong className="text-slate-800">文件 ID {sr.import_file_id.slice(0, 8)} / 第 {sr.row_number} 行</strong>
                </div>
                <div className="text-xs text-slate-400">
                  导入时间：{new Date(sr.created_at).toLocaleString()}
                </div>
              </div>

              <div className="grid grid-cols-2 sm:grid-cols-4 gap-4 text-xs">
                <div>
                  <span className="text-slate-400">原始书名：</span>
                  <div className="font-semibold text-slate-800">{sr.raw_payload.title || sr.raw_payload.bookname || sr.raw_payload.name || "-"}</div>
                </div>
                <div>
                  <span className="text-slate-400">原始作者：</span>
                  <div className="font-semibold text-slate-800">{sr.raw_payload.author || sr.raw_payload.authors || "-"}</div>
                </div>
                <div>
                  <span className="text-slate-400">原始出版社：</span>
                  <div className="font-semibold text-slate-800">{sr.raw_payload.publisher || sr.raw_payload.press || "-"}</div>
                </div>
                <div>
                  <span className="text-slate-400">原始 ISBN：</span>
                  <div className="font-semibold text-slate-800">{sr.raw_payload.isbn || sr.raw_payload.isbns || "-"}</div>
                </div>
              </div>

              <div className="pt-2">
                <details className="text-xs text-slate-500">
                  <summary className="cursor-pointer font-medium text-blue-600 hover:text-blue-800">
                    查看该来源行完整 JSON 载荷
                  </summary>
                  <pre className="mt-2 p-3 bg-slate-900 text-slate-100 rounded-lg overflow-x-auto font-mono text-[11px]">
                    {JSON.stringify(sr.raw_payload, null, 2)}
                  </pre>
                </details>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Tab 2: 来源候选文件 */}
      {activeTab === "assets" && (
        <Card className="p-5">
          {source_assets.length === 0 ? (
            <div className="text-center py-6 text-sm text-slate-400">
              该书目来源记录中未声明任何下载候选文件。
            </div>
          ) : (
            <table className="w-full text-left text-sm">
              <thead className="border-b border-slate-200 text-xs font-semibold text-slate-500 uppercase">
                <tr>
                  <th className="pb-3">格式</th>
                  <th className="pb-3">声明大小</th>
                  <th className="pb-3">来源 MD5</th>
                  <th className="pb-3">可用状态</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-slate-100">
                {source_assets.map((asset) => (
                  <tr key={asset.id}>
                    <td className="py-3 font-mono font-bold uppercase text-blue-600">
                      {asset.format}
                    </td>
                    <td className="py-3 text-slate-600">
                      {asset.declared_size_bytes
                        ? (asset.declared_size_bytes / (1024 * 1024)).toFixed(2) + " MB"
                        : "未提供"}
                    </td>
                    <td className="py-3 font-mono text-xs text-slate-600">
                      {asset.md5 || "未提供"}
                    </td>
                    <td className="py-3">
                      <span className="px-2 py-0.5 rounded text-xs bg-emerald-50 text-emerald-700 font-medium">
                        {asset.status}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>
      )}

      {/* Tab 3: 文件资产与哈希校验 */}
      {activeTab === "holdings" && (
        <div className="space-y-4">
          {holdings.length === 0 ? (
            <Card className="p-8 text-center text-slate-500">
              暂无已下载并校验入库的物理文件实体。
            </Card>
          ) : (
            holdings.map(([holding, file]) => (
              <Card key={holding.id} className="p-5 space-y-3 border-emerald-200 bg-emerald-50/10">
                <div className="flex items-center justify-between border-b border-slate-100 pb-2">
                  <div className="flex items-center gap-2">
                    <FileCheck className="h-5 w-5 text-emerald-600" />
                    <span className="font-bold text-slate-800">
                      {file.storage_backend} 存储实体（{file.format.toUpperCase()}）
                    </span>
                    <span className="px-2 py-0.5 rounded text-xs bg-emerald-100 text-emerald-800 font-bold">
                      SHA-256 已验证
                    </span>
                  </div>
                  <div className="text-xs text-slate-400">
                    校验时间：{file.verified_at ? new Date(file.verified_at).toLocaleString() : "-"}
                  </div>
                </div>

                <div className="space-y-1.5 text-xs font-mono">
                  <div className="flex items-center gap-2 text-slate-700">
                    <span className="text-slate-400 w-24">存储对象键:</span>
                    <strong className="text-slate-900">{file.object_key}</strong>
                  </div>
                  <div className="flex items-center gap-2 text-slate-700">
                    <span className="text-slate-400 w-24">物理文件大小:</span>
                    <span>{(file.actual_size_bytes / (1024 * 1024)).toFixed(2)} MB ({file.actual_size_bytes} 字节)</span>
                  </div>
                  <div className="flex items-center gap-2 text-slate-700">
                    <span className="text-slate-400 w-24">SHA-256 证据:</span>
                    <span className="text-emerald-700 font-bold break-all">{file.sha256}</span>
                  </div>
                </div>
              </Card>
            ))
          )}
        </div>
      )}

      {/* Tab 4: 执行时间线 */}
      {activeTab === "timeline" && (
        <Card className="p-5">
          {executions.length === 0 ? (
            <div className="text-center py-6 text-sm text-slate-400">
              尚无 Worker 获取执行历史。
            </div>
          ) : (
            <div className="space-y-4">
              {executions.map((exec) => (
                <div key={exec.id} className="flex items-start gap-3 border-l-2 border-slate-200 pl-4 py-1">
                  <div className="space-y-1 text-xs">
                    <div className="flex items-center gap-2">
                      <span className="font-semibold text-slate-800">{exec.stage}</span>
                      <StatusBadge status={exec.result || "执行中"} />
                      <span className="text-slate-400 font-mono">
                        {new Date(exec.started_at).toLocaleString()}
                      </span>
                    </div>

                    {exec.error_message && (
                      <div className="text-rose-600 bg-rose-50 p-2 rounded border border-rose-200">
                        {exec.error_message}
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
