import { useEffect, useState } from "react";
import {
  CheckCircle2,
  XCircle,
  AlertTriangle,
  RefreshCw,
  FileText,
  BookOpen,
} from "lucide-react";
import {
  listInventoryReviews,
  confirmInventoryReview,
  ignoreInventoryReview,
} from "../../lib/api";
import { InventoryReviewDetail } from "../../lib/types";
import { Card, Spinner, Button } from "../../components/ui";
import { useToast } from "../../context/ToastContext";

export function InventoryReviewPage() {
  const [reviews, setReviews] = useState<InventoryReviewDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [processingId, setProcessingId] = useState<string | null>(null);

  const { success, error: toastError } = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const res = await listInventoryReviews();
      setReviews(res.reviews || []);
    } catch (err: any) {
      setError(err.message || "加载待确认馆藏列表失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const handleConfirm = async (entryId: string, editionId: string, editionTitle: string) => {
    if (!confirm(`确定要将该文件关联到版本《${editionTitle}》吗？这将立即将对应获取任务更新为“已下载”。`)) {
      return;
    }

    try {
      setProcessingId(entryId);
      await confirmInventoryReview(entryId, editionId);
      success("候选已成功确认并关联馆藏");
      setReviews((prev) => prev.filter((r) => r.id !== entryId));
    } catch (err: any) {
      toastError(err.message || "确认候选失败");
    } finally {
      setProcessingId(null);
    }
  };

  const handleIgnore = async (entryId: string) => {
    if (!confirm("确定要忽略该待确认文件吗？")) return;

    try {
      setProcessingId(entryId);
      await ignoreInventoryReview(entryId);
      success("已标记忽略");
      setReviews((prev) => prev.filter((r) => r.id !== entryId));
    } catch (err: any) {
      toastError(err.message || "忽略失败");
    } finally {
      setProcessingId(null);
    }
  };

  if (loading && reviews.length === 0) {
    return <Spinner label="正在加载待确认馆藏..." />;
  }

  return (
    <div className="space-y-6">
      {/* 顶部标题 */}
      <div className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-4">
        <div>
          <h1 className="text-2xl font-bold text-gray-900 flex items-center gap-2">
            <AlertTriangle className="w-7 h-7 text-amber-500" />
            待确认馆藏审核
          </h1>
          <p className="text-sm text-gray-500 mt-1">
            处理扫描过程中多候选、字段冲突或低置信度的文件，确认后将自动登记馆藏并收敛为“已下载”
          </p>
        </div>
        <Button variant="secondary" onClick={loadData}>
          <RefreshCw className="w-4 h-4 mr-1.5 inline" />
          刷新
        </Button>
      </div>

      {error && (
        <div className="p-4 bg-red-50 border border-red-200 rounded-lg text-sm text-red-700">
          {error}
        </div>
      )}

      {/* 待确认条目列表 */}
      <div className="space-y-4">
        {reviews.map((item) => (
          <Card key={item.id} className="p-5 border-l-4 border-l-amber-400">
            <div className="flex flex-col md:flex-row justify-between items-start md:items-center gap-4 border-b border-gray-100 pb-3">
              <div>
                <div className="flex items-center gap-2">
                  <FileText className="w-5 h-5 text-gray-500" />
                  <h3 className="font-semibold text-gray-900 text-base">{item.file_name}</h3>
                  <span className="px-2 py-0.5 rounded text-xs uppercase font-medium bg-gray-100 text-gray-700">
                    {item.extension}
                  </span>
                </div>
                <p className="text-xs text-gray-500 font-mono mt-1">
                  路径: {item.object_key} · 大小: {(item.actual_size_bytes / (1024 * 1024)).toFixed(2)} MB · SHA256: {item.sha256.slice(0, 16)}...
                </p>
                {item.error_reason && (
                  <p className="text-xs text-amber-600 mt-1">
                    提示: {item.error_reason}
                  </p>
                )}
              </div>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => handleIgnore(item.id)}
                disabled={processingId === item.id}
                className="text-gray-500 hover:text-red-600"
              >
                <XCircle className="w-4 h-4 mr-1 inline" />
                忽略该文件
              </Button>
            </div>

            {/* 候选版本列表 */}
            <div className="mt-4">
              <h4 className="text-xs font-semibold text-gray-700 mb-2 uppercase tracking-wide">
                候选书目版本 ({item.candidates.length})
              </h4>
              <div className="space-y-2">
                {item.candidates.map((cand) => (
                  <div
                    key={cand.candidate_id}
                    className="flex flex-col sm:flex-row justify-between items-start sm:items-center p-3 rounded-lg bg-gray-50/80 hover:bg-gray-100/80 transition-colors border border-gray-200/60"
                  >
                    <div className="space-y-1">
                      <div className="flex items-center gap-2">
                        <BookOpen className="w-4 h-4 text-primary-600" />
                        <span className="font-medium text-gray-900 text-sm">{cand.edition_title || "未知书名"}</span>
                        <span className="px-1.5 py-0.5 text-xs font-semibold bg-primary-100 text-primary-800 rounded">
                          得分: {cand.match_score}
                        </span>
                      </div>
                      <div className="text-xs text-gray-500">
                        {cand.publisher && <span>出版社: {cand.publisher} </span>}
                        {cand.publish_year && <span>({cand.publish_year}年)</span>}
                        <span className="ml-2 font-mono text-gray-400">ID: {cand.edition_id.slice(0, 8)}</span>
                      </div>
                    </div>
                    <div className="mt-2 sm:mt-0">
                      <Button
                        size="sm"
                        variant="primary"
                        onClick={() => handleConfirm(item.id, cand.edition_id, cand.edition_title)}
                        disabled={processingId === item.id}
                      >
                        <CheckCircle2 className="w-4 h-4 mr-1 inline" />
                        确认为此书
                      </Button>
                    </div>
                  </div>
                ))}
                {item.candidates.length === 0 && (
                  <div className="p-4 text-center text-xs text-gray-400 bg-gray-50 rounded-lg">
                    未自动计算出候选，可保持未匹配或在导入书单后自动重新匹配
                  </div>
                )}
              </div>
            </div>
          </Card>
        ))}

        {reviews.length === 0 && (
          <div className="p-12 text-center bg-gray-50 rounded-lg border border-dashed border-gray-200">
            <CheckCircle2 className="w-10 h-10 text-green-500 mx-auto mb-2" />
            <h3 className="text-sm font-semibold text-gray-900">当前没有待确认的馆藏</h3>
            <p className="text-xs text-gray-500 mt-1">所有扫描发现的文件均已成功自动匹配或进入未匹配池</p>
          </div>
        )}
      </div>
    </div>
  );
}
