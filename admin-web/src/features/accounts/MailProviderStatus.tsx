import { useEffect, useState } from "react";
import { getMailProviderStatus } from "../../lib/api";
import type { MailProviderStatus as ProviderStatus } from "../../lib/types";
import { Badge } from "../../components/ui";

export function MailProviderStatus() {
  const [config, setConfig] = useState<ProviderStatus | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let mounted = true;
    getMailProviderStatus()
      .then((cfg) => {
        if (mounted) setConfig(cfg);
      })
      .catch(() => {})
      .finally(() => {
        if (mounted) setLoading(false);
      });

    return () => {
      mounted = false;
    };
  }, []);

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-xs text-slate-500">
        <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-slate-300" />
        <span>检测邮件接收服务状态...</span>
      </div>
    );
  }

  const isOutlook = config?.provider_type === "outlook_http";
  const isMock = config?.provider_type === "mock";
  const isManual = !config || config.provider_type === "manual";
  const outlookReady = Boolean(
    isOutlook &&
      config?.is_active &&
      config.has_api_key &&
      config.health === "Worker 已全部应用",
  );

  return (
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-slate-200 bg-slate-50/70 px-4 py-3 text-sm">
      <div className="flex items-center gap-3">
        <div className="flex items-center gap-2 font-medium text-slate-800">
          <span
            className={`inline-block h-2.5 w-2.5 rounded-full ${
              outlookReady
                ? "bg-emerald-500"
                : isOutlook || isMock
                  ? "bg-amber-500"
                  : "bg-blue-500"
            }`}
          />
          <span>邮件验证码服务：</span>
        </div>

        {isOutlook && (
          <div className="flex items-center gap-2">
            <Badge variant={outlookReady ? "success" : "warning"}>Outlook HTTP 自动提取</Badge>
            <span className="text-xs text-slate-500">
              版本 v{config.version} · {config.has_api_key ? "已配密钥" : "无密钥"} · {config.health}
            </span>
          </div>
        )}

        {isMock && (
          <div className="flex items-center gap-2">
            <Badge variant="warning">Mock 测试桩 (非生产)</Badge>
            <span className="text-xs text-slate-500">版本 v{config?.version || 1}</span>
          </div>
        )}

        {isManual && (
          <div className="flex items-center gap-2">
            <Badge variant="info">人工输入模式 (Manual)</Badge>
            <span className="text-xs text-slate-500">当收到验证码时，系统将生成待办并由人工确认填写</span>
          </div>
        )}
      </div>

      <div className="text-xs text-slate-500">
        Worker 应用 {config?.workers_applied ?? 0}/{config?.workers_online ?? 0} · {" "}
        可在 <a href="/system/settings#mail-provider" className="text-blue-600 hover:underline">系统设置</a> 中配置或热切换 Provider
      </div>
    </div>
  );
}
