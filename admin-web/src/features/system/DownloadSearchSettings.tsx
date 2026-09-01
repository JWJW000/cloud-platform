import { useEffect, useState } from "react";
import { getDownloadSearchOptions, updateDownloadSearchOptions } from "../../lib/api";
import { useToast } from "../../context/ToastContext";
import { Button, Card, CardHeader, Input, Spinner } from "../../components/ui";

export function DownloadSearchSettings({ canManage }: { canManage: boolean }) {
  const toast = useToast();
  const [order, setOrder] = useState("bestmatch");
  const [extensions, setExtensions] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);

  const load = async () => {
    setLoading(true);
    try {
      const options = await getDownloadSearchOptions();
      setOrder(options.order);
      setExtensions(options.extensions.join(", "));
    } catch (error: any) {
      toast.error(error?.message || "读取下载搜索参数失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const save = async () => {
    if (!canManage) return;
    const normalizedOrder = order.trim();
    if (!normalizedOrder) {
      toast.error("order 不能为空");
      return;
    }
    const normalizedExtensions = extensions
      .split(/[,，\s]+/)
      .map((item) => item.trim())
      .filter(Boolean);

    setSaving(true);
    try {
      const updated = await updateDownloadSearchOptions({
        order: normalizedOrder,
        extensions: normalizedExtensions,
      });
      setOrder(updated.order);
      setExtensions(updated.extensions.join(", "));
      toast.success("下载搜索参数已保存，并已推送给在线 Worker");
    } catch (error: any) {
      toast.error(error?.message || "保存下载搜索参数失败");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card className="overflow-hidden border border-slate-200">
      <CardHeader
        title="下载搜索参数"
        description="控制图书站点搜索 URL 中的 order 与 extensions 参数；新建搜索任务立即生效"
      />
      {loading ? (
        <div className="p-6"><Spinner label="加载下载搜索参数中..." /></div>
      ) : (
        <div className="space-y-4 border-t border-slate-100 p-5">
          <Input
            label="排序参数（order）"
            value={order}
            disabled={!canManage}
            placeholder="bestmatch"
            onChange={(event) => setOrder(event.target.value)}
          />
          <Input
            label="扩展名（extensions，逗号分隔）"
            value={extensions}
            disabled={!canManage}
            placeholder="留空则按任务格式自动使用 pdf 或 epub"
            onChange={(event) => setExtensions(event.target.value)}
          />
          <div className="rounded-md border border-blue-100 bg-blue-50 px-3 py-2 text-xs leading-5 text-blue-800">
            示例：填写 <span className="font-mono">pdf, epub</span> 会生成
            <span className="font-mono"> extensions[0]=pdf&amp;extensions[1]=epub</span>。
            留空保持原行为，由每个任务的目标格式自动填充。
          </div>
          {canManage && (
            <div className="flex justify-end">
              <Button loading={saving} onClick={save}>保存搜索参数</Button>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}
