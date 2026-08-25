import { useState, useEffect } from "react";
import {
  getMailProviderConfig,
  getMailProviderStatus,
  updateMailProviderConfig,
  testMailProvider,
} from "../../lib/api";
import type {
  MailProviderConfig,
  MailProviderStatus,
  TestMailProviderResult,
} from "../../lib/types";
import { useToast } from "../../context/ToastContext";
import { Button, Card, CardHeader, Input, Select, Badge, Spinner } from "../../components/ui";

export function MailCodeSettings({ canManage }: { canManage: boolean }) {
  const toast = useToast();
  const [config, setConfig] = useState<MailProviderConfig | null>(null);
  const [providerStatus, setProviderStatus] = useState<MailProviderStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<TestMailProviderResult | null>(null);

  // Form states
  const [providerType, setProviderType] = useState<string>("manual");
  const [endpoint, setEndpoint] = useState<string>("");
  const [apiKey, setApiKey] = useState<string>("");
  const [pollIntervalSecs, setPollIntervalSecs] = useState<number>(5);
  const [timeoutSecs, setTimeoutSecs] = useState<number>(60);
  const [allowedHostsText, setAllowedHostsText] = useState<string>("");
  const [allowedSendersText, setAllowedSendersText] = useState<string>("");

  const loadConfig = async () => {
    setLoading(true);
    try {
      const [data, status] = await Promise.all([
        getMailProviderConfig(),
        getMailProviderStatus().catch(() => null),
      ]);
      setProviderStatus(status);
      if (data) {
        setConfig(data);
        setProviderType(data.provider_type);
        setEndpoint(data.endpoint || "");
        setPollIntervalSecs(data.poll_interval_secs || 5);
        setTimeoutSecs(data.timeout_secs || 60);
        setAllowedHostsText((data.allowed_hosts || []).join(", "));
        setAllowedSendersText((data.allowed_senders || []).join(", "));
      }
    } catch {
      toast.error("获取邮件 Provider 配置失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const allowed_hosts = allowedHostsText
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      if (providerType === "outlook_http" && allowed_hosts.length === 0) {
        toast.error("Outlook HTTP 模式必须填写允许的主机名白名单");
        return;
      }

      const res = await testMailProvider({
        provider_type: providerType,
        endpoint,
        api_key: apiKey || undefined,
        allowed_hosts,
      });
      setTestResult(res);
      if (res.success) {
        toast.success(res.message);
      } else {
        toast.error(res.message);
      }
    } catch (e: any) {
      setTestResult({
        success: false,
        message: e?.message || "连通性测试请求失败",
      });
      toast.error("测试请求失败");
    } finally {
      setTesting(false);
    }
  };

  const handleSave = async () => {
    if (!canManage) return;
    setSaving(true);
    try {
      const allowed_hosts = allowedHostsText
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      if (providerType === "outlook_http" && allowed_hosts.length === 0) {
        toast.error("Outlook HTTP 模式必须填写允许的主机名白名单");
        return;
      }

      const saved = await updateMailProviderConfig({
        provider_type: providerType,
        endpoint,
        api_key: apiKey || undefined,
        poll_interval_secs: pollIntervalSecs,
        timeout_secs: timeoutSecs,
        allowed_hosts,
        allowed_senders: allowedSendersText.split(",").map((s) => s.trim()).filter(Boolean),
      });

      setConfig(saved);
      setApiKey(""); // 密钥只写，保存后清空输入框
      setProviderStatus(await getMailProviderStatus().catch(() => null));
      toast.success(`邮件 Provider 配置已热更新为版本 v${saved.version}`);
    } catch (e: any) {
      toast.error(e?.message || "更新配置失败");
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <Spinner label="正在加载邮件 Provider 设置..." />;
  }

  return (
    <Card id="mail-provider">
      <CardHeader
        title="邮件验证码 Provider 与自动取码设置"
        description="配置注册流程中接收邮箱验证码的适配器及安全参数。支持热切换，运行中的任务保持配置快照隔离。"
      />
      <div className="space-y-5 p-5">
        {/* 当前状态与版本 */}
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-slate-200 bg-slate-50 p-4">
          <div>
            <div className="text-sm font-semibold text-slate-800">当前激活版本</div>
            <div className="mt-1 flex items-center gap-2">
              <Badge
                variant={
                  config?.provider_type === "outlook_http" &&
                  providerStatus?.health === "Worker 已全部应用"
                    ? "success"
                    : config?.provider_type === "outlook_http"
                      ? "warning"
                      : "info"
                }
              >
                {config?.provider_type === "outlook_http"
                  ? "Outlook HTTP 自动取码"
                  : config?.provider_type === "mock"
                  ? "Mock 测试桩"
                  : "Manual 人工输入"}
              </Badge>
              <span className="text-xs text-slate-500">
                版本: v{config?.version || 1} · 更新人: {config?.updated_by || "系统"} ·{" "}
                {config?.updated_at ? new Date(config.updated_at).toLocaleString() : "-"}
              </span>
            </div>
          </div>
          <div className="text-xs text-slate-500">
            <div>
              {config?.has_api_key ? (
                <span className="text-emerald-600 font-medium">✓ API Key 已配置 (受保护存储)</span>
              ) : (
                <span className="text-slate-400">未设置 API Key</span>
              )}
            </div>
            <div className="mt-1 text-right">
              {providerStatus?.health ?? "等待状态上报"} · Worker 应用 {providerStatus?.workers_applied ?? 0}/
              {providerStatus?.workers_online ?? 0}
            </div>
          </div>
        </div>

        {/* 表单配置 */}
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">Provider 类型</label>
            <Select
              value={providerType}
              onChange={(e) => setProviderType(e.target.value)}
              disabled={!canManage || saving}
            >
              <option value="manual">人工输入模式 (Manual - 产生待办事项)</option>
              <option value="outlook_http">Outlook HTTP 自动提取 (外部安全邮件服务)</option>
              <option value="mock">Mock 模拟测试桩 (非生产环境)</option>
            </Select>
            <p className="mt-1 text-[11px] text-slate-500">
              当配置为 Outlook HTTP 时，Worker 会在注册遇到验证码时自动拉取并解析邮件。
            </p>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">服务 API 端点 (HTTPS 限制)</label>
            <Input
              type="text"
              placeholder="https://mail-service.internal.domain/api/external/emails"
              value={endpoint}
              onChange={(e) => setEndpoint(e.target.value)}
              disabled={!canManage || saving || providerType !== "outlook_http"}
            />
            <p className="mt-1 text-[11px] text-slate-500">
              强制要求 HTTPS 协议，Master 与 Worker 均会对 IP 进行防 SSRF 与私网过滤校验。
            </p>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">
              API Key / 鉴权令牌 <span className="text-slate-400 font-normal">(只写不读)</span>
            </label>
            <Input
              type="password"
              placeholder={config?.has_api_key ? "•••••••••••• (如需修改请输入新密钥)" : "请输入 API Key"}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              disabled={!canManage || saving || providerType !== "outlook_http"}
            />
            <p className="mt-1 text-[11px] text-slate-500">
              密钥绝不会回显至前端或存储于明文配置，更新时写入安全引用。
            </p>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">允许的主机名白名单 (逗号分隔)</label>
            <Input
              type="text"
              placeholder="mail.example.com, api.mailhub.org"
              value={allowedHostsText}
              onChange={(e) => setAllowedHostsText(e.target.value)}
              disabled={!canManage || saving || providerType !== "outlook_http"}
            />
            <p className="mt-1 text-[11px] text-slate-500">Outlook HTTP 模式必填；仅白名单中的 HTTPS 主机可被访问，并同时执行私网/元数据 IP 拦截。</p>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">允许的验证码发件人（逗号分隔）</label>
            <Input
              type="text"
              placeholder="no-reply@example.com"
              value={allowedSendersText}
              onChange={(e) => setAllowedSendersText(e.target.value)}
              disabled={!canManage || saving || providerType !== "outlook_http"}
            />
            <p className="mt-1 text-[11px] text-slate-500">填写后只接受这些发件人的新邮件；建议生产环境明确配置。</p>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">轮询间隔 (秒)</label>
            <Input
              type="number"
              min={1}
              max={60}
              value={pollIntervalSecs}
              onChange={(e) => setPollIntervalSecs(parseInt(e.target.value, 10) || 5)}
              disabled={!canManage || saving || providerType !== "outlook_http"}
            />
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-slate-700">收取超时时间 (秒)</label>
            <Input
              type="number"
              min={10}
              max={300}
              value={timeoutSecs}
              onChange={(e) => setTimeoutSecs(parseInt(e.target.value, 10) || 60)}
              disabled={!canManage || saving || providerType !== "outlook_http"}
            />
          </div>
        </div>

        {/* 测试结果提示 */}
        {testResult && (
          <div
            className={`rounded-md p-3 text-xs ${
              testResult.success
                ? "bg-emerald-50 text-emerald-800 border border-emerald-200"
                : "bg-red-50 text-red-800 border border-red-200"
            }`}
          >
            <div className="font-semibold">{testResult.success ? "连通性测试通过" : "连通性测试失败"}</div>
            <div className="mt-1">{testResult.message}</div>
            {testResult.latency_ms !== undefined && testResult.latency_ms !== null && (
              <div className="mt-0.5 text-slate-500">响应延迟: {testResult.latency_ms} ms</div>
            )}
          </div>
        )}

        {/* 操作按钮 */}
        <div className="flex items-center justify-end gap-3 border-t border-slate-100 pt-4">
          <Button
            variant="secondary"
            onClick={handleTest}
            disabled={testing || saving || (providerType === "outlook_http" && !endpoint)}
          >
            {testing ? "正在测试连通性..." : "测试连接 (SSRF 校验)"}
          </Button>
          {canManage && (
            <Button variant="primary" onClick={handleSave} disabled={saving || testing}>
              {saving ? "正在更新配置..." : "保存并发布新版本"}
            </Button>
          )}
        </div>
      </div>
    </Card>
  );
}
