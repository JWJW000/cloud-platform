import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  AlertCircle,
  GitMerge,
  Sparkles,
} from "lucide-react";
import { getCatalogStats, mergeCatalogWorks, searchCatalog } from "../lib/api";
import { CatalogSearchResponse, CatalogStats } from "../lib/types";
import { Card, Spinner, Button, Input } from "../components/ui";
import { useToast } from "../context/ToastContext";

export function CatalogQualityPage() {
  const [stats, setStats] = useState<CatalogStats | null>(null);
  const [ambiguousList, setAmbiguousList] = useState<CatalogSearchResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 合并表单状态
  const [sourceWorkId, setSourceWorkId] = useState("");
  const [targetWorkId, setTargetWorkId] = useState("");
  const [merging, setMerging] = useState(false);

  const { success, error: toastError } = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [s, amb] = await Promise.all([
        getCatalogStats(),
        searchCatalog({ limit: 10, offset: 0 }),
      ]);
      setStats(s);
      setAmbiguousList(amb);
    } catch (err: any) {
      setError(err.message || "加载数据质量指标失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const handleMerge = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!sourceWorkId.trim() || !targetWorkId.trim()) {
      toastError("请填写源作品编号与目标作品编号");
      return;
    }
    try {
      setMerging(true);
      await mergeCatalogWorks(sourceWorkId.trim(), targetWorkId.trim());
      success("作品合并成功，相关版本已重新挂接");
      setSourceWorkId("");
      setTargetWorkId("");
      loadData();
    } catch (err: any) {
      toastError(err.message || "合并失败");
    } finally {
      setMerging(false);
    }
  };

  if (loading && !stats) {
    return <Spinner label="正在计算总库数据质量与消歧状态..." />;
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-xl font-bold text-slate-900">数据质量与书目消歧治理</h1>
        <p className="text-xs text-slate-500">
          缺失元数据治理、多源冲突仲裁、人工图书合并与版本消歧。
        </p>
      </div>

      {error && (
        <div className="rounded-lg bg-red-50 p-4 border border-red-200 text-sm text-red-700 flex items-center gap-2">
          <AlertCircle className="h-5 w-5" />
          {error}
        </div>
      )}

      {/* 质量指标面板 */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <Card className="p-5">
          <div className="text-xs font-semibold text-slate-500 uppercase">待消歧作品</div>
          <div className="mt-2 text-2xl font-bold text-amber-600">
            {stats?.ambiguous_works_count.toLocaleString() || 0}
          </div>
          <div className="mt-1 text-xs text-slate-400">信息不足或有多候选</div>
        </Card>

        <Card className="p-5">
          <div className="text-xs font-semibold text-slate-500 uppercase">缺失 ISBN 版本</div>
          <div className="mt-2 text-2xl font-bold text-slate-800">
            {stats?.missing_isbn_count.toLocaleString() || 0}
          </div>
          <div className="mt-1 text-xs text-slate-400">仅靠题名+责任者对齐</div>
        </Card>

        <Card className="p-5">
          <div className="text-xs font-semibold text-slate-500 uppercase">缺失责任者版本</div>
          <div className="mt-2 text-2xl font-bold text-slate-800">
            {stats?.missing_author_count.toLocaleString() || 0}
          </div>
          <div className="mt-1 text-xs text-slate-400">来源未填作者姓名</div>
        </Card>

        <Card className="p-5">
          <div className="text-xs font-semibold text-slate-500 uppercase">隔离区未决行</div>
          <div className="mt-2 text-2xl font-bold text-rose-600">
            {stats?.total_quarantined.toLocaleString() || 0}
          </div>
          <div className="mt-1 text-xs text-slate-400">格式异常暂存</div>
        </Card>
      </div>

      {/* 人工合并操作卡片 */}
      <Card className="p-6">
        <div className="flex items-center gap-2 border-b border-slate-100 pb-3 mb-4">
          <GitMerge className="h-5 w-5 text-indigo-600" />
          <h3 className="font-bold text-slate-900">人工作品实体合并 (Manual Merge)</h3>
        </div>
        <p className="text-xs text-slate-500 mb-4">
          当两本作品经确认属于同一著作时，可将其合并。源作品下的所有出版版本、来源记录和馆藏文件将安全转移至目标作品，源作品保留合并指向且不破坏原始来源出处。
        </p>

        <form onSubmit={handleMerge} className="grid grid-cols-1 sm:grid-cols-3 gap-4 items-end">
          <div>
            <label className="block text-xs font-semibold text-slate-700 mb-1">源作品 UUID（将被合并）</label>
            <Input
              value={sourceWorkId}
              onChange={(e) => setSourceWorkId(e.target.value)}
              placeholder="输入源作品 work_id..."
              className="text-xs font-mono"
            />
          </div>

          <div>
            <label className="block text-xs font-semibold text-slate-700 mb-1">目标作品 UUID（保留正本）</label>
            <Input
              value={targetWorkId}
              onChange={(e) => setTargetWorkId(e.target.value)}
              placeholder="输入目标作品 work_id..."
              className="text-xs font-mono"
            />
          </div>

          <Button type="submit" variant="primary" disabled={merging}>
            {merging ? "正在合并..." : "执行实体合并"}
          </Button>
        </form>
      </Card>

      {/* 待消歧与核对样本 */}
      <Card className="p-5">
        <div className="flex items-center justify-between border-b border-slate-100 pb-3 mb-4">
          <div className="flex items-center gap-2">
            <Sparkles className="h-5 w-5 text-amber-600" />
            <h3 className="font-semibold text-slate-900">书目样本快速核对</h3>
          </div>
          <Link to="/catalog/search" className="text-xs text-blue-600 hover:text-blue-800 font-medium">
            进入高级检索 →
          </Link>
        </div>

        <div className="divide-y divide-slate-100">
          {ambiguousList?.items.map((item) => (
            <div key={item.id} className="py-3 flex items-center justify-between">
              <div>
                <Link
                  to={`/catalog/editions/${item.id}`}
                  className="font-bold text-slate-900 hover:text-blue-600 text-sm"
                >
                  {item.title}
                </Link>
                <div className="text-xs text-slate-500 mt-0.5">
                  作品 UUID: <span className="font-mono">{item.work_id}</span> | 作者: {item.authors.join(", ") || "未提供"} | 出版社: {item.publisher || "未知"}
                </div>
              </div>
              <div className="flex items-center gap-2">
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => setSourceWorkId(item.work_id)}
                >
                  设为合并源
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => setTargetWorkId(item.work_id)}
                >
                  设为目标
                </Button>
              </div>
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}
