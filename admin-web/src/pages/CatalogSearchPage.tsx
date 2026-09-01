import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import {
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Filter,
  Layers,
  Search,
  Tag,
  AlertCircle,
  FileCheck,
} from "lucide-react";
import { searchCatalog } from "../lib/api";
import { CatalogSearchResponse } from "../lib/types";
import { Card, Skeleton, StatusBadge, Button, Input } from "../components/ui";

export function CatalogSearchPage() {
  const [searchParams, setSearchParams] = useSearchParams();

  const query = searchParams.get("query") || "";
  const acquisition_status = searchParams.get("status") || "";
  const work_type = searchParams.get("work_type") || "";
  const language = searchParams.get("language") || "";
  const format = searchParams.get("format") || "";
  const cursor = searchParams.get("cursor") || "";
  const limit = 20;

  const [inputQuery, setInputQuery] = useState(query);
  const [data, setData] = useState<CatalogSearchResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchResults = async () => {
    try {
      setLoading(true);
      setError(null);
      const res = await searchCatalog({
        query: query || undefined,
        acquisition_status: acquisition_status || undefined,
        work_type: work_type || undefined,
        language: language || undefined,
        format: format || undefined,
        limit,
        cursor: cursor || undefined,
      });
      setData(res);
    } catch (err: any) {
      setError(err.message || "检索我的书目总库失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchResults();
  }, [query, acquisition_status, work_type, language, format, cursor]);

  const updateParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(searchParams);
    if (value) {
      next.set(key, value);
    } else {
      next.delete(key);
    }
    next.delete("offset");
    next.delete("cursor");
    setSearchParams(next);
  };

  const moveToCursor = (value: string | null | undefined) => {
    const next = new URLSearchParams(searchParams);
    next.delete("offset");
    if (value) next.set("cursor", value);
    else next.delete("cursor");
    setSearchParams(next);
  };

  const handleSearchSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    updateParam("query", inputQuery.trim() || null);
  };

  return (
    <div className="flex flex-col h-full space-y-4 overflow-hidden">
      <div className="shrink-0">
        <h1 className="text-xl font-bold text-slate-900">我的书目总库检索</h1>
        <p className="text-xs text-slate-500">
          多源归并后的规范书目检索：支持题名、作者、ISBN/DOI、来源编号与全维度分面过滤。
        </p>
      </div>

      {/* 顶部搜索条 */}
      <Card className="p-4 shrink-0 shadow-sm">
        <form onSubmit={handleSearchSubmit} className="flex gap-3">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-3 h-4 w-4 text-slate-400" />
            <Input
              value={inputQuery}
              onChange={(e) => setInputQuery(e.target.value)}
              placeholder="输入书名、作者、出版社、ISBN、DOI 或来源编号进行检索..."
              className="pl-9"
            />
          </div>
          <Button type="submit" variant="primary">
            检索
          </Button>
          {(query || acquisition_status || work_type || language || format) && (
            <Button
              type="button"
              variant="secondary"
              onClick={() => {
                setInputQuery("");
                setSearchParams({});
              }}
            >
              重置筛选
            </Button>
          )}
        </form>
      </Card>

      {error && (
        <div className="shrink-0 rounded-lg bg-red-50 p-4 border border-red-200 text-sm text-red-700 flex items-center gap-2">
          <AlertCircle className="h-5 w-5" />
          {error}
        </div>
      )}

      {/* 检索内容区（左侧分面，右侧结果）：填满剩余高度，页内独立滚动 */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-6 flex-1 min-h-0 overflow-hidden">
        {/* 左侧分面过滤器（自身独立滚动） */}
        <div className="md:col-span-1 h-full overflow-y-auto space-y-4 pr-1">
          <Card className="p-4 space-y-4">
            <div>
              <div className="flex items-center gap-2 text-xs font-bold text-slate-700 uppercase tracking-wider mb-2">
                <Filter className="h-3.5 w-3.5" />
                获取状态
              </div>
              <div className="space-y-1">
                {[
                  { label: "全部状态", val: "" },
                  { label: "总库已拥有", val: "总库已拥有" },
                  { label: "文件已归档", val: "已下载" },
                  { label: "正在下载 / 校验", val: "下载中" },
                  { label: "暂时失败 / 重试", val: "暂时失败" },
                  { label: "待人工确认", val: "人工确认" },
                ].map((item) => (
                  <button
                    key={item.val}
                    type="button"
                    onClick={() => updateParam("status", item.val || null)}
                    className={`w-full text-left px-2.5 py-1.5 rounded-md text-xs transition flex items-center justify-between ${
                      acquisition_status === item.val
                        ? "bg-blue-50 text-blue-700 font-semibold"
                        : "text-slate-600 hover:bg-slate-50"
                    }`}
                  >
                    <span>{item.label}</span>
                    {acquisition_status === item.val && (
                      <CheckCircle2 className="h-3.5 w-3.5 text-blue-600" />
                    )}
                  </button>
                ))}
              </div>
            </div>

            <div className="border-t border-slate-100 pt-3">
              <div className="flex items-center gap-2 text-xs font-bold text-slate-700 uppercase tracking-wider mb-2">
                <Layers className="h-3.5 w-3.5" />
                作品类型
              </div>
              <div className="space-y-1">
                {[
                  { label: "全部类型", val: "" },
                  { label: "整书专著", val: "整书" },
                  { label: "专著章节", val: "章节" },
                  { label: "论文", val: "论文" },
                  { label: "合集", val: "合集" },
                ].map((item) => (
                  <button
                    key={item.val}
                    type="button"
                    onClick={() => updateParam("work_type", item.val || null)}
                    className={`w-full text-left px-2.5 py-1.5 rounded-md text-xs transition flex items-center justify-between ${
                      work_type === item.val
                        ? "bg-blue-50 text-blue-700 font-semibold"
                        : "text-slate-600 hover:bg-slate-50"
                    }`}
                  >
                    <span>{item.label}</span>
                  </button>
                ))}
              </div>
            </div>

            <div className="border-t border-slate-100 pt-3">
              <div className="flex items-center gap-2 text-xs font-bold text-slate-700 uppercase tracking-wider mb-2">
                <Tag className="h-3.5 w-3.5" />
                语种过滤
              </div>
              <div className="space-y-1">
                {[
                  { label: "全部语种", val: "" },
                  { label: "中文 (zh)", val: "zh" },
                  { label: "英文 (en)", val: "en" },
                  { label: "其他 (ot)", val: "ot" },
                ].map((item) => (
                  <button
                    key={item.val}
                    type="button"
                    onClick={() => updateParam("language", item.val || null)}
                    className={`w-full text-left px-2.5 py-1.5 rounded-md text-xs transition flex items-center justify-between ${
                      language === item.val
                        ? "bg-blue-50 text-blue-700 font-semibold"
                        : "text-slate-600 hover:bg-slate-50"
                    }`}
                  >
                    <span>{item.label}</span>
                  </button>
                ))}
              </div>
            </div>
          </Card>
        </div>

        {/* 右侧结果列表（自身独立纵向滚动） */}
        <div className="md:col-span-3 h-full flex flex-col min-h-0 overflow-hidden">
          <div className="flex items-center justify-between text-xs text-slate-500 pb-2 shrink-0">
            <span>
              {query
                ? (data?.next_cursor ? "已找到匹配结果（超过 1,000 条）" : `已找到匹配结果（共 ${data?.items.length || 0} 条）`)
                : `总库现有约 ${(data?.total || 0).toLocaleString()} 条记录`}
            </span>
            <span>{cursor ? "游标分页" : "首屏结果"}</span>
          </div>

          <div className="flex-1 min-h-0 overflow-y-auto space-y-3 pr-1">
            {loading && !data ? (
              <div className="space-y-3">
                {Array.from({ length: 5 }).map((_, idx) => (
                  <Card key={idx} className="p-4 space-y-3 shadow-sm">
                    <div className="flex items-center gap-2">
                      <Skeleton className="h-5 w-48" />
                      <Skeleton className="h-4 w-12" />
                    </div>
                    <div className="flex gap-4">
                      <Skeleton className="h-3.5 w-32" />
                      <Skeleton className="h-3.5 w-36" />
                      <Skeleton className="h-3.5 w-20" />
                    </div>
                    <Skeleton className="h-3 w-64" />
                  </Card>
                ))}
              </div>
            ) : data?.items.length === 0 ? (
              <Card className="p-12 text-center text-slate-500">
                未找到匹配的书目记录，请尝试调整检索关键词或分面筛选条件。
              </Card>
            ) : (
              data?.items.map((item) => (
                <Card key={item.id} className="p-4 hover:border-blue-300 transition shadow-sm">
                  <div className="flex items-start justify-between gap-4">
                    <div className="space-y-1.5 flex-1">
                      <div className="flex items-center gap-2">
                        <Link
                          to={`/library/editions/${item.id}`}
                          className="font-bold text-slate-900 hover:text-blue-600 text-base"
                        >
                          {item.title}
                        </Link>
                        <span className="text-[11px] px-1.5 py-0.5 rounded bg-slate-100 text-slate-600 font-medium">
                          {item.work_type}
                        </span>
                        {item.resolution_status === "待消歧" && (
                          <span className="text-[11px] px-1.5 py-0.5 rounded bg-amber-100 text-amber-800 font-medium">
                            待消歧
                          </span>
                        )}
                      </div>

                      <div className="text-xs text-slate-600 flex flex-wrap items-center gap-x-4 gap-y-1">
                        <span>
                          作者：
                          <strong className="text-slate-800">
                            {item.authors.length > 0 ? item.authors.join(", ") : "未知"}
                          </strong>
                        </span>
                        <span>
                          出版社：
                          {item.publisher_id ? (
                            <Link
                              to={`/publishers/${item.publisher_id}`}
                              className="font-semibold text-blue-600 hover:underline ml-1"
                            >
                              {item.publisher}
                            </Link>
                          ) : (
                            <strong className="text-slate-800 ml-1">
                              {item.publisher || "未知"}
                            </strong>
                          )}
                        </span>
                        {item.publish_year && (
                          <span>
                            年份：<strong>{item.publish_year}</strong>
                          </span>
                        )}
                        <span>
                          语种：<strong className="uppercase">{item.language}</strong>
                        </span>
                      </div>

                      {item.identifiers.length > 0 && (
                        <div className="text-xs text-slate-500 flex items-center gap-1.5 font-mono">
                          <span>ISBN/标识符：</span>
                          {item.identifiers.slice(0, 3).map((id, idx) => (
                            <span key={idx} className="bg-slate-100 px-1.5 py-0.5 rounded">
                              {id}
                            </span>
                          ))}
                        </div>
                      )}

                      <div className="flex items-center gap-4 text-xs pt-1">
                        <div className="flex items-center gap-1.5">
                          <span className="text-slate-400">来源候选格式：</span>
                          {item.source_formats.length > 0 ? (
                            item.source_formats.map((f, i) => (
                              <span key={i} className="px-1.5 py-0.5 bg-blue-50 text-blue-700 rounded font-mono text-[11px]">
                                {f}
                              </span>
                            ))
                          ) : (
                            <span className="text-slate-400">无来源文件</span>
                          )}
                        </div>

                        <div className="flex items-center gap-1.5">
                          <span className="text-slate-400">当前可用文件：</span>
                          {item.holding_formats.length > 0 ? (
                            item.holding_formats.map((f, i) => (
                              <span key={i} className="px-1.5 py-0.5 bg-emerald-50 text-emerald-700 font-bold rounded font-mono text-[11px] flex items-center gap-0.5">
                                <FileCheck className="h-3 w-3" />
                                {f}
                              </span>
                            ))
                          ) : (
                            <span className="text-slate-400">仅书目，暂无归档文件</span>
                          )}
                        </div>
                      </div>
                    </div>

                    <div className="flex flex-col items-end gap-2 shrink-0">
                      <StatusBadge status={item.acquisition_status} />
                      <Link
                        to={`/library/editions/${item.id}`}
                        className="text-xs text-blue-600 hover:text-blue-800 font-medium"
                      >
                        详情与溯源 →
                      </Link>
                    </div>
                  </div>
                </Card>
              ))
            )}
          </div>

          {/* 分页控制器（固定底部） */}
          {data && (data.previous_cursor || data.next_cursor) && (
            <div className="flex items-center justify-between pt-3 border-t border-slate-200 shrink-0 bg-white">
              <Button
                variant="secondary"
                size="sm"
                disabled={!data.previous_cursor}
                onClick={() => moveToCursor(data.previous_cursor)}
              >
                <ChevronLeft className="h-4 w-4 mr-1" />
                上一页
              </Button>

              <span className="text-xs text-slate-500">
                键集游标分页（无延迟极速流式）
              </span>

              <Button
                variant="secondary"
                size="sm"
                disabled={!data.next_cursor}
                onClick={() => moveToCursor(data.next_cursor)}
              >
                下一页
                <ChevronRight className="h-4 w-4 ml-1" />
              </Button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
