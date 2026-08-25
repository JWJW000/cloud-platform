import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import {
  AlertCircle,
  ChevronLeft,
  ChevronRight,
  Search,
} from "lucide-react";
import { listCatalogAcquisitions, retryCatalogAcquisition } from "../lib/api";
import { CatalogSearchResponse } from "../lib/types";
import { Card, Spinner, Button, Input } from "../components/ui";
import { useToast } from "../context/ToastContext";

export function CatalogAcquisitionsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const query = searchParams.get("query") || "";
  const acquisition_status = searchParams.get("status") || "";
  const offset = parseInt(searchParams.get("offset") || "0", 10);
  const limit = 20;

  const [inputQuery, setInputQuery] = useState(query);
  const [data, setData] = useState<CatalogSearchResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { success, error: toastError } = useToast();

  const fetchResults = async () => {
    try {
      setLoading(true);
      setError(null);
      const res = await listCatalogAcquisitions({
        query: query || undefined,
        acquisition_status: acquisition_status || undefined,
        limit,
        offset,
      });
      setData(res);
    } catch (err: any) {
      setError(err.message || "获取全局任务池失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchResults();
  }, [query, acquisition_status, offset]);

  const updateParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(searchParams);
    if (value) {
      next.set(key, value);
    } else {
      next.delete(key);
    }
    next.set("offset", "0");
    setSearchParams(next);
  };

  const handleSearchSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    updateParam("query", inputQuery.trim() || null);
  };

  const handleRetry = async (targetId: string) => {
    try {
      await retryCatalogAcquisition(targetId);
      success("任务已重置为待下载");
      fetchResults();
    } catch (err: any) {
      toastError(err.message || "重试失败");
    }
  };

  const totalPages = data ? Math.ceil(data.total / limit) : 0;
  const currentPage = Math.floor(offset / limit) + 1;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-bold text-slate-900">唯一持续全局获取池</h1>
        <p className="text-xs text-slate-500">
          所有书目统一汇入同一个全局下载池：支持优先级调整、失败退避与自动候选轮转。
        </p>
      </div>

      {/* 搜索与快捷过滤 */}
      <Card className="p-4">
        <form onSubmit={handleSearchSubmit} className="flex gap-3">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-3 h-4 w-4 text-slate-400" />
            <Input
              value={inputQuery}
              onChange={(e) => setInputQuery(e.target.value)}
              placeholder="搜索待获取书名、ISBN 或作者..."
              className="pl-9"
            />
          </div>
          <Button type="submit" variant="primary">
            筛选
          </Button>
          {(query || acquisition_status) && (
            <Button
              type="button"
              variant="secondary"
              onClick={() => {
                setInputQuery("");
                setSearchParams({});
              }}
            >
              重置
            </Button>
          )}
        </form>

        <div className="flex flex-wrap items-center gap-2 pt-3 border-t border-slate-100 mt-3">
          <span className="text-xs text-slate-400 mr-1">快捷状态：</span>
          {[
            { label: "全部", val: "" },
            { label: "待下载", val: "待下载" },
            { label: "正在下载/校验", val: "下载中" },
            { label: "暂时失败", val: "暂时失败" },
            { label: "待人工确认", val: "人工确认" },
            { label: "已下载", val: "已下载" },
          ].map((item) => (
            <button
              key={item.val}
              onClick={() => updateParam("status", item.val || null)}
              className={`px-2.5 py-1 rounded-md text-xs transition ${
                acquisition_status === item.val
                  ? "bg-blue-600 text-white font-semibold shadow-sm"
                  : "bg-slate-100 text-slate-600 hover:bg-slate-200"
              }`}
            >
              {item.label}
            </button>
          ))}
        </div>
      </Card>

      {error && (
        <div className="rounded-lg bg-red-50 p-4 border border-red-200 text-sm text-red-700 flex items-center gap-2">
          <AlertCircle className="h-5 w-5" />
          {error}
        </div>
      )}

      {/* 任务列表 */}
      <Card className="p-0 overflow-hidden">
        {loading ? (
          <div className="p-12 text-center">
            <Spinner label="正在读取全局获取任务池..." />
          </div>
        ) : data?.items.length === 0 ? (
          <div className="p-12 text-center text-slate-500 text-sm">
            当前筛选条件下暂无获取任务。
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead className="border-b border-slate-200 bg-slate-50/70 text-xs font-semibold text-slate-500 uppercase">
                <tr>
                  <th className="py-3 px-4">书名 / 版本</th>
                  <th className="py-3 px-4">责任者</th>
                  <th className="py-3 px-4">出版社 / 年份</th>
                  <th className="py-3 px-4">来源格式</th>
                  <th className="py-3 px-4">获取状态</th>
                  <th className="py-3 px-4 text-right">操作</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-slate-100">
                {data?.items.map((item) => (
                  <tr key={item.id} className="hover:bg-slate-50/50">
                    <td className="py-3 px-4">
                      <Link
                        to={`/catalog/editions/${item.id}`}
                        className="font-bold text-slate-900 hover:text-blue-600"
                      >
                        {item.title}
                      </Link>
                      {item.identifiers.length > 0 && (
                        <div className="text-[11px] text-slate-400 font-mono">
                          {item.identifiers[0]}
                        </div>
                      )}
                    </td>
                    <td className="py-3 px-4 text-slate-600 text-xs">
                      {item.authors.length > 0 ? item.authors.join(", ") : "未提供"}
                    </td>
                    <td className="py-3 px-4 text-slate-600 text-xs">
                      {item.publisher || "未知"} {item.publish_year ? `(${item.publish_year})` : ""}
                    </td>
                    <td className="py-3 px-4">
                      <div className="flex gap-1">
                        {item.source_formats.map((f, i) => (
                          <span key={i} className="px-1.5 py-0.5 bg-slate-100 text-slate-700 rounded font-mono text-[11px]">
                            {f}
                          </span>
                        ))}
                      </div>
                    </td>
                    <td className="py-3 px-4">
                      <span
                        className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${
                          item.acquisition_status === "已下载"
                            ? "bg-emerald-50 text-emerald-700"
                            : item.acquisition_status === "下载中"
                            ? "bg-blue-50 text-blue-700"
                            : item.acquisition_status === "暂时失败"
                            ? "bg-amber-50 text-amber-700"
                            : "bg-slate-100 text-slate-700"
                        }`}
                      >
                        {item.acquisition_status}
                      </span>
                    </td>
                    <td className="py-3 px-4 text-right">
                      <div className="flex items-center justify-end gap-2">
                        {item.acquisition_status !== "已下载" && (
                          <Button size="sm" variant="secondary" onClick={() => handleRetry(item.id)}>
                            重试
                          </Button>
                        )}
                        <Link
                          to={`/catalog/editions/${item.id}`}
                          className="text-xs text-blue-600 hover:text-blue-800 font-medium"
                        >
                          详情
                        </Link>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {/* 分页 */}
        {data && data.total > limit && (
          <div className="p-4 border-t border-slate-200 flex items-center justify-between">
            <Button
              variant="secondary"
              size="sm"
              disabled={offset === 0}
              onClick={() => updateParam("offset", String(Math.max(0, offset - limit)))}
            >
              <ChevronLeft className="h-4 w-4 mr-1" />
              上一页
            </Button>
            <span className="text-xs text-slate-500">
              第 {currentPage} / {totalPages} 页 (共 {data.total} 条)
            </span>
            <Button
              variant="secondary"
              size="sm"
              disabled={offset + limit >= data.total}
              onClick={() => updateParam("offset", String(offset + limit))}
            >
              下一页
              <ChevronRight className="h-4 w-4 ml-1" />
            </Button>
          </div>
        )}
      </Card>
    </div>
  );
}
