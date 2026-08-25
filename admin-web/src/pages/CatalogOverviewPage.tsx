import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  BookOpen,
  CheckCircle2,
  Database,
  FileSearch,
  HardDrive,
  UploadCloud,
  AlertCircle,
  TrendingUp,
} from "lucide-react";
import { getCatalogStats, listCatalogImportRuns } from "../lib/api";
import { CatalogStats, ImportRun } from "../lib/types";
import { Card, CardHeader, Spinner } from "../components/ui";

export function CatalogOverviewPage() {
  const [stats, setStats] = useState<CatalogStats | null>(null);
  const [recentRuns, setRecentRuns] = useState<ImportRun[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [s, runs] = await Promise.all([
        getCatalogStats(),
        listCatalogImportRuns(),
      ]);
      setStats(s);
      setRecentRuns(runs.slice(0, 5));
    } catch (err: any) {
      setError(err.message || "加载总库统计失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  if (loading && !stats) {
    return <Spinner label="正在汇总总库与索引全局指标..." />;
  }

  const formatBytes = (bytes: number) => {
    if (bytes === 0) return "0 B";
    const k = 1024;
    const sizes = ["B", "KB", "MB", "GB", "TB"];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + " " + sizes[i];
  };

  const totalAcq =
    (stats?.acquired_targets || 0) +
    (stats?.pending_targets || 0) +
    (stats?.downloading_targets || 0) +
    (stats?.failed_targets || 0) +
    (stats?.needs_confirm_targets || 0);

  const acqRate =
    totalAcq > 0
      ? (((stats?.acquired_targets || 0) / totalAcq) * 100).toFixed(1)
      : "0.0";

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-slate-900">图书馆总库与索引总览</h1>
          <p className="text-xs text-slate-500">
            云端唯一图书事实库：全量书目去重聚合、馆藏文件证据核验与持续全局获取调度。
          </p>
        </div>
        <div className="flex items-center gap-3">
          <Link
            to="/catalog/imports"
            className="inline-flex items-center gap-1.5 rounded-md bg-white border border-slate-300 px-3 py-1.5 text-sm font-medium text-slate-700 hover:bg-slate-50 shadow-sm"
          >
            <UploadCloud className="h-4 w-4 text-blue-600" />
            增量导入中心
          </Link>
          <Link
            to="/catalog/search"
            className="inline-flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-700 shadow-sm"
          >
            <FileSearch className="h-4 w-4" />
            图书高级检索
          </Link>
        </div>
      </div>

      {error && (
        <div className="rounded-lg bg-red-50 p-4 border border-red-200 text-sm text-red-700 flex items-center gap-2">
          <AlertCircle className="h-5 w-5" />
          {error}
        </div>
      )}

      {/* 核心指标统计卡片 */}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <Card className="p-5">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold text-slate-500 uppercase tracking-wider">
              原始来源记录
            </span>
            <div className="p-2 bg-blue-50 text-blue-600 rounded-lg">
              <Database className="h-5 w-5" />
            </div>
          </div>
          <div className="mt-3 text-3xl font-bold text-slate-900">
            {stats?.total_source_records.toLocaleString() || 0}
          </div>
          <div className="mt-2 text-xs text-slate-500 flex items-center gap-1">
            <span>覆盖 {stats?.total_sources || 0} 个数据源清单</span>
          </div>
        </Card>

        <Card className="p-5">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold text-slate-500 uppercase tracking-wider">
              规范作品 / 版本
            </span>
            <div className="p-2 bg-emerald-50 text-emerald-600 rounded-lg">
              <BookOpen className="h-5 w-5" />
            </div>
          </div>
          <div className="mt-3 text-3xl font-bold text-slate-900">
            {stats?.total_works.toLocaleString() || 0}
          </div>
          <div className="mt-2 text-xs text-slate-500 flex items-center justify-between">
            <span>共 {stats?.total_editions.toLocaleString() || 0} 个独立出版版本</span>
            {stats?.total_chapters ? (
              <span className="text-slate-400">含 {stats.total_chapters} 章节</span>
            ) : null}
          </div>
        </Card>

        <Card className="p-5">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold text-slate-500 uppercase tracking-wider">
              馆藏下载满足率
            </span>
            <div className="p-2 bg-purple-50 text-purple-600 rounded-lg">
              <CheckCircle2 className="h-5 w-5" />
            </div>
          </div>
          <div className="mt-3 text-3xl font-bold text-slate-900">
            {acqRate}%
          </div>
          <div className="mt-2 text-xs text-slate-500 flex items-center justify-between">
            <span className="text-emerald-600 font-medium">
              {stats?.acquired_targets.toLocaleString() || 0} 已满足
            </span>
            <span>待补齐 {stats?.pending_targets.toLocaleString() || 0}</span>
          </div>
        </Card>

        <Card className="p-5">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold text-slate-500 uppercase tracking-wider">
              经过校验的馆藏资产
            </span>
            <div className="p-2 bg-amber-50 text-amber-600 rounded-lg">
              <HardDrive className="h-5 w-5" />
            </div>
          </div>
          <div className="mt-3 text-3xl font-bold text-slate-900">
            {stats?.total_library_files.toLocaleString() || 0}
          </div>
          <div className="mt-2 text-xs text-slate-500">
            已占用存储：{formatBytes(stats?.total_library_bytes || 0)}
          </div>
        </Card>
      </div>

      {/* 获取进度与数据质量监控 */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* 全局获取调度池分布 */}
        <Card className="p-5 lg:col-span-2">
          <CardHeader
            title="全局获取调度池分布"
            action={
              <Link
                to="/catalog/acquisitions"
                className="text-xs text-blue-600 hover:text-blue-800 font-medium"
              >
                查看全局任务池 →
              </Link>
            }
          />

          <div className="grid grid-cols-2 sm:grid-cols-5 gap-3 text-center mt-4">
            <div className="p-3 bg-slate-50 rounded-lg border border-slate-200">
              <div className="text-xs text-slate-500">待下载 / 排队</div>
              <div className="text-xl font-bold text-slate-800 mt-1">
                {stats?.pending_targets.toLocaleString() || 0}
              </div>
            </div>
            <div className="p-3 bg-blue-50 rounded-lg border border-blue-200">
              <div className="text-xs text-blue-600">正在执行</div>
              <div className="text-xl font-bold text-blue-700 mt-1">
                {stats?.downloading_targets.toLocaleString() || 0}
              </div>
            </div>
            <div className="p-3 bg-emerald-50 rounded-lg border border-emerald-200">
              <div className="text-xs text-emerald-600">已校验下载</div>
              <div className="text-xl font-bold text-emerald-700 mt-1">
                {stats?.acquired_targets.toLocaleString() || 0}
              </div>
            </div>
            <div className="p-3 bg-amber-50 rounded-lg border border-amber-200">
              <div className="text-xs text-amber-600">暂时失败/重试中</div>
              <div className="text-xl font-bold text-amber-700 mt-1">
                {stats?.failed_targets.toLocaleString() || 0}
              </div>
            </div>
            <div className="p-3 bg-rose-50 rounded-lg border border-rose-200">
              <div className="text-xs text-rose-600">待人工确认</div>
              <div className="text-xl font-bold text-rose-700 mt-1">
                {stats?.needs_confirm_targets.toLocaleString() || 0}
              </div>
            </div>
          </div>

          <div className="mt-4 pt-4 border-t border-slate-100 flex items-center justify-between text-xs text-slate-500">
            <span>调度策略：全库单一持续任务池，无需按批次新建任务。</span>
            <span className="text-emerald-600 font-medium">
              证据闭环：SHA-256 核验入库后自动收敛为「已下载」
            </span>
          </div>
        </Card>

        {/* 数据质量与消歧监控 */}
        <Card className="p-5">
          <CardHeader
            title="数据质量与消歧"
            action={
              <Link
                to="/catalog/quality"
                className="text-xs text-indigo-600 hover:text-indigo-800 font-medium"
              >
                治理中心 →
              </Link>
            }
          />

          <div className="space-y-3 text-sm mt-4">
            <div className="flex items-center justify-between p-2.5 rounded-lg bg-slate-50">
              <span className="text-slate-600">待消歧作品</span>
              <span className="font-semibold text-amber-600">
                {stats?.ambiguous_works_count.toLocaleString() || 0}
              </span>
            </div>
            <div className="flex items-center justify-between p-2.5 rounded-lg bg-slate-50">
              <span className="text-slate-600">缺失 ISBN 版本</span>
              <span className="font-semibold text-slate-700">
                {stats?.missing_isbn_count.toLocaleString() || 0}
              </span>
            </div>
            <div className="flex items-center justify-between p-2.5 rounded-lg bg-slate-50">
              <span className="text-slate-600">缺失作者信息</span>
              <span className="font-semibold text-slate-700">
                {stats?.missing_author_count.toLocaleString() || 0}
              </span>
            </div>
            <div className="flex items-center justify-between p-2.5 rounded-lg bg-slate-50">
              <span className="text-slate-600">隔离区异常行</span>
              <span className="font-semibold text-rose-600">
                {stats?.total_quarantined.toLocaleString() || 0}
              </span>
            </div>
          </div>
        </Card>
      </div>

      {/* 最近导入运行记录 */}
      <Card className="p-5">
        <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4">
          <div className="flex items-center gap-2">
            <TrendingUp className="h-5 w-5 text-blue-600" />
            <h3 className="font-semibold text-slate-900">最近数据导入运行</h3>
          </div>
          <Link
            to="/catalog/imports"
            className="text-xs text-blue-600 hover:text-blue-800 font-medium"
          >
            全部导入记录 →
          </Link>
        </div>

        {recentRuns.length === 0 ? (
          <div className="text-center py-8 text-sm text-slate-400">
            暂无导入运行记录，可在导入中心进行预检与增量提交。
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead className="border-b border-slate-200 text-xs font-semibold text-slate-500 uppercase">
                <tr>
                  <th className="pb-3">运行编号</th>
                  <th className="pb-3">状态</th>
                  <th className="pb-3">总行数</th>
                  <th className="pb-3">成功导入</th>
                  <th className="pb-3">重复跳过</th>
                  <th className="pb-3">隔离行</th>
                  <th className="pb-3">创建时间</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-slate-100">
                {recentRuns.map((run) => (
                  <tr key={run.id} className="hover:bg-slate-50/50">
                    <td className="py-3 font-mono text-xs text-slate-700">
                      {run.id.slice(0, 8)}...
                    </td>
                    <td className="py-3">
                      <span
                        className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                          run.status === "已完成"
                            ? "bg-emerald-50 text-emerald-700"
                            : run.status === "运行中"
                            ? "bg-blue-50 text-blue-700"
                            : "bg-amber-50 text-amber-700"
                        }`}
                      >
                        {run.status}
                      </span>
                    </td>
                    <td className="py-3 text-slate-700">
                      {run.total_rows.toLocaleString()}
                    </td>
                    <td className="py-3 text-emerald-600 font-medium">
                      {run.imported_count.toLocaleString()}
                    </td>
                    <td className="py-3 text-slate-500">
                      {run.duplicate_count.toLocaleString()}
                    </td>
                    <td className="py-3 text-rose-600 font-medium">
                      {run.quarantined_count.toLocaleString()}
                    </td>
                    <td className="py-3 text-xs text-slate-400">
                      {new Date(run.created_at).toLocaleString()}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
