import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { AlertCircle, GitMerge, Sparkles } from "lucide-react";
import {
  getCatalogStats,
  mergeCatalogWorks,
  previewCatalogWorksMerge,
  searchCatalog,
  type MergeImpactItem,
} from "../lib/api";
import type { CatalogSearchResponse, CatalogStats, EditionSearchItem } from "../lib/types";
import { Card, Spinner, Button } from "../components/ui";
import { useToast } from "../context/ToastContext";

export function CatalogQualityPage() {
  const [stats, setStats] = useState<CatalogStats | null>(null);
  const [candidates, setCandidates] = useState<CatalogSearchResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [source, setSource] = useState<EditionSearchItem | null>(null);
  const [target, setTarget] = useState<EditionSearchItem | null>(null);
  const [impact, setImpact] = useState<{ source: MergeImpactItem; target: MergeImpactItem } | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [merging, setMerging] = useState(false);
  const { success, error: toastError } = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [nextStats, result] = await Promise.all([
        getCatalogStats(),
        searchCatalog({ limit: 20, resolution_status: "待消歧" }),
      ]);
      setStats(nextStats);
      setCandidates(result);
    } catch (caught: any) {
      setError(caught.message || "加载数据质量指标失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const chooseSource = (item: EditionSearchItem) => {
    setSource(item);
    if (target?.work_id === item.work_id) setTarget(null);
    setImpact(null);
  };

  const chooseTarget = (item: EditionSearchItem) => {
    setTarget(item);
    if (source?.work_id === item.work_id) setSource(null);
    setImpact(null);
  };

  const loadImpact = async () => {
    if (!source || !target) {
      toastError("请从候选列表分别选择被合并作品和保留正本");
      return;
    }
    try {
      setPreviewing(true);
      setImpact(await previewCatalogWorksMerge(source.work_id, target.work_id));
    } catch (caught: any) {
      toastError(caught.message || "无法预览合并影响");
    } finally {
      setPreviewing(false);
    }
  };

  const handleMerge = async () => {
    if (!source || !target || !impact) return;
    if (!window.confirm(`确认将《${impact.source.title}》合并到《${impact.target.title}》？此操作会重新挂接版本、来源与文件资产。`)) {
      return;
    }
    try {
      setMerging(true);
      await mergeCatalogWorks(source.work_id, target.work_id);
      success("作品合并成功，版本、来源和文件资产已重新挂接");
      setSource(null);
      setTarget(null);
      setImpact(null);
      loadData();
    } catch (caught: any) {
      toastError(caught.message || "合并失败");
    } finally {
      setMerging(false);
    }
  };

  if (loading && !stats) return <Spinner label="正在计算总库数据质量与消歧状态..." />;

  const metrics = [
    ["待消歧作品", stats?.ambiguous_works_count ?? 0, "text-amber-600", "信息不足或存在多候选"],
    ["缺失 ISBN 版本", stats?.missing_isbn_count ?? 0, "text-slate-800", "依赖题名与责任者对齐"],
    ["缺失责任者版本", stats?.missing_author_count ?? 0, "text-slate-800", "来源未提供责任者"],
    ["隔离区未决行", stats?.total_quarantined ?? 0, "text-rose-600", "格式异常等待修复"],
  ] as const;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-bold text-slate-900">数据质量与书目消歧治理</h1>
        <p className="text-xs text-slate-500">选择疑似重复作品，并排核对后预览影响并确认合并。</p>
      </div>

      {error && (
        <div className="flex items-center gap-2 rounded-lg border border-red-200 bg-red-50 p-4 text-sm text-red-700">
          <AlertCircle className="h-5 w-5" />{error}
        </div>
      )}

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        {metrics.map(([label, value, color, description]) => (
          <Card className="p-5" key={label}>
            <div className="text-xs font-semibold uppercase text-slate-500">{label}</div>
            <div className={`mt-2 text-2xl font-bold ${color}`}>{value.toLocaleString()}</div>
            <div className="mt-1 text-xs text-slate-400">{description}</div>
          </Card>
        ))}
      </div>

      <Card className="p-5">
        <div className="mb-4 flex items-center gap-2 border-b border-slate-100 pb-3">
          <GitMerge className="h-5 w-5 text-indigo-600" />
          <h3 className="font-semibold text-slate-900">合并核对台</h3>
        </div>
        <div className="grid gap-4 md:grid-cols-2">
          <SelectionCard label="被合并作品" item={source} tone="rose" />
          <SelectionCard label="保留正本" item={target} tone="emerald" />
        </div>
        {impact && (
          <div className="mt-4 grid gap-4 rounded-lg border border-indigo-200 bg-indigo-50/50 p-4 md:grid-cols-2">
            <ImpactCard label="将迁移" item={impact.source} />
            <ImpactCard label="合并后正本" item={impact.target} />
          </div>
        )}
        <div className="mt-4 flex justify-end gap-2">
          <Button variant="secondary" disabled={!source || !target || previewing} onClick={loadImpact}>
            {previewing ? "计算影响中..." : "预览影响"}
          </Button>
          <Button disabled={!impact || merging} onClick={handleMerge}>
            {merging ? "正在合并..." : "二次确认并合并"}
          </Button>
        </div>
      </Card>

      <Card className="p-5">
        <div className="mb-3 flex items-center justify-between border-b border-slate-100 pb-3">
          <div className="flex items-center gap-2">
            <Sparkles className="h-5 w-5 text-amber-600" />
            <h3 className="font-semibold text-slate-900">候选书目</h3>
          </div>
          <Link to="/library" className="text-xs font-medium text-blue-600">进入总库检索 →</Link>
        </div>
        <div className="divide-y divide-slate-100">
          {candidates?.items.length === 0 && (
            <div className="py-8 text-center text-sm text-slate-400">当前没有待消歧候选作品</div>
          )}
          {candidates?.items.map((item) => (
            <div key={item.id} className="flex flex-col gap-3 py-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="min-w-0">
                <Link to={`/library/editions/${item.id}`} className="font-semibold text-slate-900 hover:text-blue-600">
                  {item.title}
                </Link>
                <div className="mt-1 text-xs text-slate-500">
                  {item.authors.join(", ") || "未提供作者"} · {item.publisher || "未知出版社"} · {item.identifiers[0] || "无标识符"}
                </div>
              </div>
              <div className="flex shrink-0 gap-2">
                <Button size="sm" variant={source?.work_id === item.work_id ? "danger" : "secondary"} onClick={() => chooseSource(item)}>
                  {source?.work_id === item.work_id ? "已选为被合并" : "选为被合并"}
                </Button>
                <Button size="sm" variant={target?.work_id === item.work_id ? "success" : "secondary"} onClick={() => chooseTarget(item)}>
                  {target?.work_id === item.work_id ? "已选为正本" : "选为正本"}
                </Button>
              </div>
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}

function SelectionCard({ label, item, tone }: { label: string; item: EditionSearchItem | null; tone: "rose" | "emerald" }) {
  return (
    <div className={`rounded-lg border p-4 ${tone === "rose" ? "border-rose-200 bg-rose-50/50" : "border-emerald-200 bg-emerald-50/50"}`}>
      <div className="text-xs font-semibold text-slate-500">{label}</div>
      {item ? (
        <div className="mt-2">
          <div className="font-semibold text-slate-900">{item.title}</div>
          <div className="mt-1 text-xs text-slate-500">{item.authors.join(", ") || "未提供作者"} · {item.publisher || "未知出版社"}</div>
        </div>
      ) : <div className="mt-2 text-sm text-slate-400">请从下方候选书目选择</div>}
    </div>
  );
}

function ImpactCard({ label, item }: { label: string; item: MergeImpactItem }) {
  return (
    <div>
      <div className="text-xs font-semibold text-indigo-700">{label} · {item.title}</div>
      <div className="mt-2 flex gap-4 text-xs text-slate-600">
        <span>版本 {item.editions}</span><span>来源 {item.source_records}</span><span>文件资产 {item.holdings}</span>
      </div>
    </div>
  );
}
