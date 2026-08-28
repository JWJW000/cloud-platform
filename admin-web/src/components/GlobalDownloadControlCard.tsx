import { useState } from "react";
import { CirclePause, CirclePlay } from "lucide-react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can, deniedMessage } from "../lib/permissions";
import { getGlobalDownloadControl, updateGlobalDownloadControl } from "../lib/api";
import { ApiError } from "../lib/types";
import { Button, Card, StatusBadge } from "./ui";

/**
 * 全局图书下载调度控制。
 *
 * 页面只需放置本模块；状态加载、权限、切换、错误反馈和安全收尾说明均封装在此。
 */
export function GlobalDownloadControlCard() {
  const { user } = useAuth();
  const toast = useToast();
  const { data: control, loading, error, reload } = useApi(getGlobalDownloadControl);
  const [updating, setUpdating] = useState(false);
  const canManage = can(user?.role, "manage_batch");

  const toggle = async () => {
    if (!control) return;
    setUpdating(true);
    try {
      const updated = await updateGlobalDownloadControl(!control.paused);
      toast.success(updated.paused ? "已全局暂停图书下载" : "已恢复全局图书下载");
      reload();
    } catch (cause) {
      toast.error(cause instanceof ApiError ? cause.message : "全局下载状态切换失败");
    } finally {
      setUpdating(false);
    }
  };

  return (
    <Card>
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="flex items-start gap-3">
          {control?.paused ? (
            <CirclePause className="mt-0.5 h-5 w-5 text-amber-600" />
          ) : (
            <CirclePlay className="mt-0.5 h-5 w-5 text-emerald-600" />
          )}
          <div>
            <div className="flex items-center gap-2">
              <span className="font-medium text-slate-900">全局下载调度</span>
              {control ? <StatusBadge status={control.paused ? "已暂停" : "执行中"} /> : null}
            </div>
            <p className="mt-1 text-sm text-slate-500">
              {loading
                ? "正在读取全局下载状态…"
                : error
                  ? `状态读取失败：${error}`
                  : control?.paused
                    ? `已停止派发新任务；${control.running_tasks} 个执行中任务正在安全收尾`
                    : "所有执行中的下载批次均可向在线 Worker 派发任务"}
            </p>
          </div>
        </div>
        {error ? (
          <Button variant="secondary" size="sm" onClick={reload}>
            重试
          </Button>
        ) : canManage ? (
          <Button
            variant={control?.paused ? "success" : "secondary"}
            loading={updating}
            disabled={!control || loading}
            onClick={toggle}
          >
            {control?.paused ? "恢复全局下载" : "全局暂停下载"}
          </Button>
        ) : (
          <span className="text-sm text-slate-400" title={deniedMessage("manage_batch")}>
            只读
          </span>
        )}
      </div>
    </Card>
  );
}
