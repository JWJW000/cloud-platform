import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  BookOpen,
  CheckCircle2,
  Database,
  FileSearch,
  UploadCloud,
  AlertCircle,
  TrendingUp,
  Clock,
  ArrowRight,
} from "lucide-react";
import { getCatalogStats, listCatalogImportRuns, api } from "../../lib/api";
import { CatalogStats, ImportRun, Overview } from "../../lib/types";
import { formatBytes, formatTime } from "../../lib/format";
import { Card, Skeleton, SkeletonCard } from "../../components/ui";

interface RecentExecution {
  id: string;
  task_id: string;
  task_type: string;
  result: string;
  started_at: string;
  finished_at: string;
  duration_ms: number;
}

export function OverviewPage() {
  const [stats, setStats] = useState<CatalogStats | null>(null);
  const [overview, setOverview] = useState<Overview | null>(null);
  const [recentRuns, setRecentRuns] = useState<ImportRun[]>([]);
  const [recentExecs, setRecentExecs] = useState<RecentExecution[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [s, o, runs, execs] = await Promise.all([
        getCatalogStats().catch(() => null),
        api.get<Overview>("/api/overview").catch(() => null),
        listCatalogImportRuns().catch(() => []),
        api.get<RecentExecution[]>("/api/overview/recent-executions").catch(() => []),
      ]);
      setStats(s);
      setOverview(o);
      setRecentRuns(runs ? runs.slice(0, 5) : []);
      setRecentExecs(execs ? execs.slice(0, 5) : []);
    } catch (err: any) {
      setError(err.message || "加载总览数据失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const formatSize = (bytes: number) => {
    if (!bytes || bytes === 0) return "0 B";
    return formatBytes(bytes);
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
      {/* 顶栏标题与快捷入口 */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-slate-900">数字图书馆总览</h1>
          <p className="text-xs text-slate-500">
            全量规范图书总库、持续全局获取池、集群资源与运行健康状况
          </p>
        </div>
        <div className="flex items-center gap-3">
          <Link
            to="/imports"
            className="inline-flex items-center gap-1.5 rounded-md bg-white border border-slate-300 px-3 py-1.5 text-sm font-medium text-slate-700 hover:bg-slate-50 shadow-sm"
          >
            <UploadCloud className="h-4 w-4 text-blue-600" />
            数据导入
          </Link>
          <Link
            to="/library"
            className="inline-flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-700 shadow-sm"
          >
            <FileSearch className="h-4 w-4" />
            检索图书
          </Link>
        </div>
      </div>

      {error && (
        <div className="rounded-lg bg-red-50 p-4 border border-red-200 text-sm text-red-700 flex items-center gap-2">
          <AlertCircle className="h-5 w-5 shrink-0" />
          {error}
        </div>
      )}

      {/* 总库补齐进度轨 */}
      <div className="rounded-xl border border-slate-200 bg-white p-5 shadow-sm">
        <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4">
          <div className="flex items-center gap-2">
            <TrendingUp className="h-4 w-4 text-blue-600" />
            <h2 className="text-sm font-semibold text-slate-900">总库补齐进度轨</h2>
          </div>
          <div className="text-xs text-slate-500">
            获取池覆盖率:{" "}
            {loading && !stats ? (
              <Skeleton className="inline-block h-3 w-10 align-middle" />
            ) : (
              <span className="font-bold text-blue-600">{acqRate}%</span>
            )}
          </div>
        </div>

        <div className="grid grid-cols-2 md:grid-cols-6 gap-3">
          <div className="rounded-lg bg-slate-50 p-3 border border-slate-100">
            <div className="text-[11px] font-medium text-slate-500">1. 书目总库</div>
            <div className="mt-1 text-lg font-bold text-slate-900">
              {loading && !stats ? <Skeleton className="h-6 w-16" /> : (stats?.total_works?.toLocaleString() || 0)}
            </div>
            <div className="text-[11px] text-slate-400">规范作品</div>
          </div>
          <div className="rounded-lg bg-slate-50 p-3 border border-slate-100">
            <div className="text-[11px] font-medium text-slate-500">2. 来源记录</div>
            <div className="mt-1 text-lg font-bold text-slate-900">
              {loading && !stats ? <Skeleton className="h-6 w-16" /> : (stats?.total_source_records?.toLocaleString() || 0)}
            </div>
            <div className="text-[11px] text-slate-400">出处记录</div>
          </div>
          <div className="rounded-lg bg-blue-50/50 p-3 border border-blue-100">
            <div className="text-[11px] font-medium text-blue-700">3. 排队待抓</div>
            <div className="mt-1 text-lg font-bold text-blue-700">
              {loading && !stats ? <Skeleton className="h-6 w-16" /> : (stats?.pending_targets?.toLocaleString() || 0)}
            </div>
            <div className="text-[11px] text-blue-500">待调度任务</div>
          </div>
          <div className="rounded-lg bg-amber-50/50 p-3 border border-amber-100">
            <div className="text-[11px] font-medium text-amber-700">4. 获取执行中</div>
            <div className="mt-1 text-lg font-bold text-amber-700">
              {loading && !stats ? <Skeleton className="h-6 w-16" /> : (stats?.downloading_targets?.toLocaleString() || 0)}
            </div>
            <div className="text-[11px] text-amber-500">
              {overview ? `占用 ${overview.slots.running} 槽位` : "Worker 下载中"}
            </div>
          </div>
          <div className="rounded-lg bg-purple-50/50 p-3 border border-purple-100">
            <div className="text-[11px] font-medium text-purple-700">5. 馆藏记录</div>
            <div className="mt-1 text-lg font-bold text-purple-700">
              {loading && !stats ? <Skeleton className="h-6 w-16" /> : (stats?.total_holdings?.toLocaleString() || 0)}
            </div>
            <div className="text-[11px] text-purple-500">SHA-256 完好</div>
          </div>
          <div className="rounded-lg bg-green-50/50 p-3 border border-green-100">
            <div className="text-[11px] font-medium text-green-700">6. 已入总馆藏</div>
            <div className="mt-1 text-lg font-bold text-green-700">
              {loading && !stats ? <Skeleton className="h-6 w-16" /> : (stats?.acquired_targets?.toLocaleString() || 0)}
            </div>
            <div className="text-[11px] text-green-500">
              {loading && !stats ? <Skeleton className="h-3 w-12" /> : formatSize(stats?.total_library_bytes || 0)}
            </div>
          </div>
        </div>
      </div>

      {/* 核心指标卡片 */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {loading && !stats ? (
          <>
            <SkeletonCard className="border-l-4 border-l-blue-500" />
            <SkeletonCard className="border-l-4 border-l-emerald-500" />
            <SkeletonCard className="border-l-4 border-l-amber-500" />
            <SkeletonCard className="border-l-4 border-l-purple-500" />
          </>
        ) : (
          <>
            <Card className="p-4 border-l-4 border-l-blue-500">
              <div className="flex items-center justify-between text-slate-500 text-xs">
                <span className="font-medium text-slate-600">待下载书目池</span>
                <Clock className="h-4 w-4 text-blue-500" />
              </div>
              <div className="mt-2 text-2xl font-bold text-blue-600">
                {stats?.pending_targets?.toLocaleString() || 0}
              </div>
              <div className="mt-1 text-xs text-slate-400">
                排队中待调度下载
              </div>
            </Card>

            <Card className="p-4 border-l-4 border-l-emerald-500">
              <div className="flex items-center justify-between text-slate-500 text-xs">
                <span className="font-medium text-slate-600">总下载 / 已入馆</span>
                <CheckCircle2 className="h-4 w-4 text-emerald-500" />
              </div>
              <div className="mt-2 text-2xl font-bold text-emerald-600">
                {stats?.acquired_targets?.toLocaleString() || 0}
              </div>
              <div className="mt-1 text-xs text-slate-400">
                实体馆藏 {formatSize(stats?.total_library_bytes || 0)}
              </div>
            </Card>

            <Card className="p-4 border-l-4 border-l-amber-500">
              <div className="flex items-center justify-between text-slate-500 text-xs">
                <span className="font-medium text-slate-600">今日下载完成</span>
                <TrendingUp className="h-4 w-4 text-amber-500" />
              </div>
              <div className="mt-2 text-2xl font-bold text-amber-600">
                {stats?.today_downloaded_count?.toLocaleString() || 0}
              </div>
              <div className="mt-1 text-xs text-slate-400">
                今日入库 {stats?.today_added_works_count?.toLocaleString() || 0} 本书目
              </div>
            </Card>

            <Card className="p-4 border-l-4 border-l-purple-500">
              <div className="flex items-center justify-between text-slate-500 text-xs">
                <span className="font-medium text-slate-600">总库规范书目</span>
                <BookOpen className="h-4 w-4 text-purple-500" />
              </div>
              <div className="mt-2 text-2xl font-bold text-purple-600">
                {stats?.total_editions?.toLocaleString() || 0}
              </div>
              <div className="mt-1 text-xs text-slate-400">
                对应 {stats?.total_works?.toLocaleString() || 0} 部规范作品
              </div>
            </Card>
          </>
        )}
      </div>

      {/* 下半区：运行健康与最近记录 */}
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
        {/* 左侧：最近数据导入 */}
        <Card className="p-5">
          <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-3">
            <div className="flex items-center gap-2">
              <Database className="h-4 w-4 text-slate-500" />
              <h3 className="text-sm font-semibold text-slate-900">最近数据导入</h3>
            </div>
            <Link to="/imports" className="text-xs text-blue-600 hover:underline inline-flex items-center gap-1">
              查看全部 <ArrowRight className="h-3 w-3" />
            </Link>
          </div>
          {recentRuns.length === 0 ? (
            <div className="py-8 text-center text-xs text-slate-400">暂无导入记录</div>
          ) : (
            <div className="space-y-3">
              {recentRuns.map((run) => (
                <div
                  key={run.id}
                  className="flex items-center justify-between rounded-md border border-slate-100 bg-slate-50/50 p-3 text-xs"
                >
                  <div className="space-y-0.5">
                    <div className="font-medium text-slate-800">批次 #{run.id.substring(0, 8)}</div>
                    <div className="text-slate-400">
                      总数: {run.total_rows} | 成功: {run.imported_count} | 隔离: {run.quarantined_count}
                    </div>
                  </div>
                  <div className="text-right">
                    <span
                      className={`inline-block rounded px-1.5 py-0.5 font-medium ${
                        run.status === "completed"
                          ? "bg-green-100 text-green-700"
                          : run.status === "running"
                            ? "bg-blue-100 text-blue-700"
                            : "bg-red-100 text-red-700"
                      }`}
                    >
                      {run.status === "completed" ? "已完成" : run.status === "running" ? "执行中" : "异常"}
                    </span>
                    <div className="mt-1 text-[10px] text-slate-400">{formatTime(run.created_at)}</div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </Card>

        {/* 右侧：近期调度与执行动态 */}
        <Card className="p-5">
          <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-3">
            <div className="flex items-center gap-2">
              <Clock className="h-4 w-4 text-slate-500" />
              <h3 className="text-sm font-semibold text-slate-900">近期执行动态</h3>
            </div>
            <Link to="/acquisitions" className="text-xs text-blue-600 hover:underline inline-flex items-center gap-1">
              获取队列 <ArrowRight className="h-3 w-3" />
            </Link>
          </div>
          {recentExecs.length === 0 ? (
            <div className="py-8 text-center text-xs text-slate-400">暂无近期执行动态</div>
          ) : (
            <div className="space-y-2">
              {recentExecs.map((exec) => (
                <div
                  key={exec.id}
                  className="flex items-center justify-between border-b border-slate-50 py-2 text-xs last:border-0"
                >
                  <div className="space-y-0.5">
                    <div className="font-medium text-slate-700 flex items-center gap-2">
                      <span>{exec.task_type}</span>
                      <span className="text-[10px] text-slate-400 font-mono">
                        {exec.task_id ? exec.task_id.substring(0, 8) : "-"}
                      </span>
                    </div>
                    <div className="text-[10px] text-slate-400">
                      耗时 {(exec.duration_ms / 1000).toFixed(1)}s | {formatTime(exec.finished_at || exec.started_at)}
                    </div>
                  </div>
                  <span
                    className={`rounded px-1.5 py-0.5 font-medium ${
                      exec.result === "success"
                        ? "bg-green-50 text-green-700"
                        : exec.result === "failed"
                          ? "bg-red-50 text-red-700"
                          : "bg-slate-100 text-slate-600"
                    }`}
                  >
                    {exec.result === "success" ? "成功" : exec.result === "failed" ? "失败" : exec.result}
                  </span>
                </div>
              ))}
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}
