import { useEffect, useState } from "react";
import {
  FolderSearch,
  HardDrive,
  Play,
  XCircle,
  RefreshCw,
  Clock,
} from "lucide-react";
import {
  listStorageLocations,
  listInventoryScans,
  createInventoryScan,
  cancelInventoryScan,
} from "../../lib/api";
import { StorageLocation, InventoryScanJob } from "../../lib/types";
import { Card, Spinner, StatusBadge, Button } from "../../components/ui";
import { useToast } from "../../context/ToastContext";

export function InventoryScansPage() {
  const [locations, setLocations] = useState<StorageLocation[]>([]);
  const [jobs, setJobs] = useState<InventoryScanJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 新建扫描抽屉/模态框
  const [showModal, setShowModal] = useState(false);
  const [selectedLocationId, setSelectedLocationId] = useState("");
  const [scanMode, setScanMode] = useState<"增量" | "全量复核">("增量");
  const [submitting, setSubmitting] = useState(false);

  const { success, error: toastError } = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      setError(null);
      const [locRes, jobRes] = await Promise.all([
        listStorageLocations(),
        listInventoryScans(),
      ]);
      setLocations(locRes.locations || []);
      setJobs(jobRes.jobs || []);
    } catch (err: any) {
      setError(err.message || "加载文件盘点扫描数据失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
    const interval = setInterval(loadData, 5000);
    return () => clearInterval(interval);
  }, []);

  const handleStartScan = async (e: React.FormEvent) => {
    e.preventDefault();
    const loc = locations.find((l) => l.id === selectedLocationId);
    if (!loc || !loc.node_id) {
      toastError("请选择一个有效的在线存储位置");
      return;
    }

    try {
      setSubmitting(true);
      await createInventoryScan({
        node_id: loc.node_id,
        storage_location_id: loc.id,
        scan_mode: scanMode,
      });
      success("文件盘点扫描任务已成功下发");
      setShowModal(false);
      loadData();
    } catch (err: any) {
      toastError(err.message || "下发扫描任务失败");
    } finally {
      setSubmitting(false);
    }
  };

  const handleCancel = async (id: string) => {
    if (!confirm("确定要取消该扫描任务吗？")) return;
    try {
      await cancelInventoryScan(id);
      success("扫描任务已取消");
      loadData();
    } catch (err: any) {
      toastError(err.message || "取消失败");
    }
  };

  if (loading && locations.length === 0) {
    return <Spinner label="正在加载文件盘点扫描任务..." />;
  }

  return (
    <div className="space-y-6">
      {/* 顶部标题与操作 */}
      <div className="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-4">
        <div>
          <h1 className="text-2xl font-bold text-gray-900 flex items-center gap-2">
            <FolderSearch className="w-7 h-7 text-primary-600" />
            文件盘点扫描
          </h1>
          <p className="text-sm text-gray-500 mt-1">
            只读扫描 Worker 节点、移动硬盘或 NAS 中的文件，校验后关联到已拥有书目版本
          </p>
        </div>
        <div className="flex items-center gap-3">
          <Button variant="secondary" onClick={loadData}>
            <RefreshCw className="w-4 h-4 mr-1.5 inline" />
            刷新
          </Button>
          <Button
            variant="primary"
            onClick={() => setShowModal(true)}
            disabled={locations.length === 0}
          >
            <Play className="w-4 h-4 mr-1.5 inline" />
            新建扫描
          </Button>
        </div>
      </div>

      {error && (
        <div className="p-4 bg-red-50 border border-red-200 rounded-lg text-sm text-red-700">
          {error}
        </div>
      )}

      {/* 存储根目录卡片列表 */}
      <div>
        <h2 className="text-base font-semibold text-gray-900 mb-3 flex items-center gap-2">
          <HardDrive className="w-5 h-5 text-gray-500" />
          已登记的存储根目录 ({locations.length})
        </h2>
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {locations.map((loc) => (
            <Card key={loc.id} className="p-4">
              <div className="flex justify-between items-start">
                <div>
                  <h3 className="font-semibold text-gray-900">{loc.display_name}</h3>
                  <p className="text-xs text-gray-500 mt-1 font-mono">别名: {loc.root_key}</p>
                  <p className="text-xs text-gray-500">后端: {loc.backend}</p>
                </div>
                <StatusBadge status={loc.availability} />
              </div>
              <div className="mt-3 pt-3 border-t border-gray-100 flex justify-between items-center text-xs text-gray-500">
                <span>节点: {loc.node_id ? loc.node_id.slice(0, 8) : "系统默认"}</span>
                <span>{loc.last_seen_at ? new Date(loc.last_seen_at).toLocaleTimeString() : "-"}</span>
              </div>
            </Card>
          ))}
          {locations.length === 0 && (
            <div className="col-span-full p-8 text-center bg-gray-50 rounded-lg border border-dashed border-gray-200 text-gray-500 text-sm">
              暂无 Worker 登记的存储根目录，请确保 Worker 在 worker.toml 中配置了 [inventory.roots] 并已连上 Master
            </div>
          )}
        </div>
      </div>

      {/* 扫描任务列表 */}
      <div>
        <h2 className="text-base font-semibold text-gray-900 mb-3 flex items-center gap-2">
          <Clock className="w-5 h-5 text-gray-500" />
          扫描任务记录
        </h2>
        <Card className="overflow-hidden">
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm text-gray-600">
              <thead className="bg-gray-50 text-gray-700 font-semibold border-b border-gray-200">
                <tr>
                  <th className="py-3 px-4">任务编号</th>
                  <th className="py-3 px-4">扫描模式</th>
                  <th className="py-3 px-4">状态</th>
                  <th className="py-3 px-4">已发现 / 哈希</th>
                  <th className="py-3 px-4">匹配统计</th>
                  <th className="py-3 px-4">创建时间</th>
                  <th className="py-3 px-4 text-right">操作</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {jobs.map((job) => (
                  <tr key={job.id} className="hover:bg-gray-50/60 transition-colors">
                    <td className="py-3 px-4 font-mono text-xs text-gray-900">
                      {job.id.slice(0, 8)}
                    </td>
                    <td className="py-3 px-4">
                      <span className="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-blue-50 text-blue-700">
                        {job.scan_mode}
                      </span>
                    </td>
                    <td className="py-3 px-4">
                      <StatusBadge status={job.status} />
                    </td>
                    <td className="py-3 px-4 text-xs font-mono">
                      {job.discovered_count} / {job.hashed_count}
                    </td>
                    <td className="py-3 px-4 text-xs">
                      <div className="flex gap-2">
                        <span className="text-green-600 font-medium">已匹配: {job.matched_count}</span>
                        <span className="text-amber-600 font-medium">待确认: {job.review_count}</span>
                        <span className="text-gray-500">未匹配: {job.unmatched_count}</span>
                        {job.error_count > 0 && (
                          <span className="text-red-600 font-medium">错误: {job.error_count}</span>
                        )}
                      </div>
                    </td>
                    <td className="py-3 px-4 text-xs text-gray-500">
                      {new Date(job.created_at).toLocaleString()}
                    </td>
                    <td className="py-3 px-4 text-right">
                      {(job.status === "待下发" || job.status === "扫描中") && (
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => handleCancel(job.id)}
                          className="text-red-600 hover:text-red-700 hover:bg-red-50"
                        >
                          <XCircle className="w-4 h-4 mr-1 inline" />
                          取消
                        </Button>
                      )}
                    </td>
                  </tr>
                ))}
                {jobs.length === 0 && (
                  <tr>
                    <td colSpan={7} className="py-8 text-center text-gray-400">
                      暂无扫描任务记录
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </Card>
      </div>

      {/* 新建扫描模态框 */}
      {showModal && (
        <div className="fixed inset-0 z-50 bg-black/40 flex items-center justify-center p-4">
          <Card className="w-full max-w-md p-6 bg-white shadow-xl">
            <h3 className="text-lg font-bold text-gray-900 mb-4 flex items-center gap-2">
              <Play className="w-5 h-5 text-primary-600" />
              新建文件盘点扫描任务
            </h3>
            <form onSubmit={handleStartScan} className="space-y-4">
              <div>
                <label className="block text-xs font-semibold text-gray-700 mb-1">
                  选择存储位置
                </label>
                <select
                  value={selectedLocationId}
                  onChange={(e) => setSelectedLocationId(e.target.value)}
                  className="w-full text-sm border-gray-300 rounded-lg shadow-sm focus:border-primary-500 focus:ring-primary-500"
                  required
                >
                  <option value="">-- 请选择在线存储根目录 --</option>
                  {locations
                    .filter((l) => l.availability === "在线" && l.node_id)
                    .map((l) => (
                      <option key={l.id} value={l.id}>
                        {l.display_name} ({l.root_key})
                      </option>
                    ))}
                </select>
              </div>

              <div>
                <label className="block text-xs font-semibold text-gray-700 mb-1">
                  扫描模式
                </label>
                <div className="grid grid-cols-2 gap-3">
                  <label
                    className={`flex items-center justify-center p-3 border rounded-lg cursor-pointer text-sm font-medium transition-colors ${
                      scanMode === "增量"
                        ? "border-primary-500 bg-primary-50 text-primary-700"
                        : "border-gray-200 text-gray-600 hover:bg-gray-50"
                    }`}
                  >
                    <input
                      type="radio"
                      name="scanMode"
                      value="增量"
                      checked={scanMode === "增量"}
                      onChange={() => setScanMode("增量")}
                      className="sr-only"
                    />
                    增量扫描
                  </label>
                  <label
                    className={`flex items-center justify-center p-3 border rounded-lg cursor-pointer text-sm font-medium transition-colors ${
                      scanMode === "全量复核"
                        ? "border-primary-500 bg-primary-50 text-primary-700"
                        : "border-gray-200 text-gray-600 hover:bg-gray-50"
                    }`}
                  >
                    <input
                      type="radio"
                      name="scanMode"
                      value="全量复核"
                      checked={scanMode === "全量复核"}
                      onChange={() => setScanMode("全量复核")}
                      className="sr-only"
                    />
                    全量复核
                  </label>
                </div>
                <p className="text-xs text-gray-500 mt-1">
                  增量扫描快速识别新增或修改文件；全量复核会检查丢失与损坏副本。
                </p>
              </div>

              <div className="flex justify-end gap-3 pt-4 border-t border-gray-100">
                <Button
                  type="button"
                  variant="secondary"
                  onClick={() => setShowModal(false)}
                  disabled={submitting}
                >
                  取消
                </Button>
                <Button type="submit" variant="primary" loading={submitting}>
                  下发任务
                </Button>
              </div>
            </form>
          </Card>
        </div>
      )}
    </div>
  );
}
