import { useState, useEffect } from "react";
import {
  getWebhookDetails,
  updateWebhookConfig,
  sendWebhookManual,
} from "../../lib/api";
import type {
  WebhookConfig,
  WebhookPlatform,
} from "../../lib/types";
import { useToast } from "../../context/ToastContext";
import {
  Button,
  Card,
  CardHeader,
  Input,
  Select,
  Badge,
  Spinner,
} from "../../components/ui";
import { Send, Eye, CheckCircle2 } from "lucide-react";

export function WebhookSettings({ canManage }: { canManage: boolean }) {
  const toast = useToast();
  const [config, setConfig] = useState<WebhookConfig | null>(null);
  const [previewMarkdown, setPreviewMarkdown] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [sending, setSending] = useState(false);
  const [showPreview, setShowPreview] = useState(false);
  const [customNote, setCustomNote] = useState("");

  // 表单状态
  const [enabled, setEnabled] = useState(false);
  const [url, setUrl] = useState("");
  const [platform, setPlatform] = useState<WebhookPlatform>("feishu");
  const [secret, setSecret] = useState("");
  const [dailyPushTime, setDailyPushTime] = useState("20:00");
  const [titlePrefix, setTitlePrefix] = useState("「数字图书馆」每日下载日报");
  const [includeSystemStatus, setIncludeSystemStatus] = useState(true);

  const loadData = async () => {
    setLoading(true);
    try {
      const data = await getWebhookDetails();
      if (data) {
        setConfig(data.config);
        setPreviewMarkdown(data.preview_markdown);
        setEnabled(data.config.enabled);
        setUrl(data.config.url || "");
        setPlatform(data.config.platform || "feishu");
        setSecret(data.config.secret || "");
        setDailyPushTime(data.config.daily_push_time || "20:00");
        setTitlePrefix(data.config.title_prefix || "「数字图书馆」每日下载日报");
        setIncludeSystemStatus(data.config.include_system_status ?? true);
      }
    } catch {
      toast.error("获取 Webhook 推送设置失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const handleSave = async () => {
    if (!canManage) return;
    setSaving(true);
    try {
      const updated = await updateWebhookConfig({
        enabled,
        url: url.trim(),
        platform,
        secret: secret.trim() ? secret.trim() : undefined,
        daily_push_time: dailyPushTime.trim() || "20:00",
        title_prefix: titlePrefix.trim() || "「数字图书馆」每日下载日报",
        include_system_status: includeSystemStatus,
        last_pushed_date: config?.last_pushed_date,
      });
      setConfig(updated);
      toast.success("Webhook 推送配置已保存");
      loadData();
    } catch (e: any) {
      toast.error(e?.message || "保存 Webhook 配置失败");
    } finally {
      setSaving(false);
    }
  };

  const handleManualSend = async () => {
    if (!url.trim()) {
      toast.error("请先填写并保存 Webhook 推送地址");
      return;
    }
    setSending(true);
    try {
      const res = await sendWebhookManual({
        custom_note: customNote.trim() || undefined,
      });
      toast.success(res.message || "推送成功");
    } catch (e: any) {
      toast.error(e?.message || "推送失败");
    } finally {
      setSending(false);
    }
  };

  if (loading) {
    return (
      <Card className="p-6">
        <Spinner label="加载 Webhook 设置中..." />
      </Card>
    );
  }

  return (
    <Card className="overflow-hidden border border-slate-200">
      <CardHeader
        title="消息推送与日报 Webhook"
        description="支持将每日图书下载量、成功率、流量及集群健康状态定时/手动推送至飞书机器人、企业微信等群聊（Markdown 丰富排版）"
        action={
          enabled ? (
            <Badge variant="success">定时推送已启用 ({dailyPushTime})</Badge>
          ) : (
            <Badge variant="neutral">未启用定时</Badge>
          )
        }
      />

      <div className="space-y-5 p-5">
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="block text-xs font-medium text-slate-700 mb-1">
              机器人平台类型
            </label>
            <Select
              value={platform}
              onChange={(e) => setPlatform(e.target.value as WebhookPlatform)}
              disabled={!canManage}
            >
              <option value="feishu">飞书自定义机器人 (推荐，支持富文本卡片)</option>
              <option value="wechat">企业微信群机器人 (Markdown)</option>
              <option value="dingtalk">钉钉群机器人 (Markdown)</option>
              <option value="generic">通用 Webhook (JSON Payload)</option>
            </Select>
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-700 mb-1">
              每日定时推送时间 (24小时制 HH:mm)
            </label>
            <Input
              type="time"
              value={dailyPushTime}
              onChange={(e) => setDailyPushTime(e.target.value)}
              disabled={!canManage}
              placeholder="20:00"
            />
          </div>
        </div>

        <div>
          <label className="block text-xs font-medium text-slate-700 mb-1">
            Webhook URL 地址
          </label>
          <Input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            disabled={!canManage}
            placeholder="https://open.feishu.cn/open-apis/bot/v2/hook/xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
          />
          <p className="text-[11px] text-slate-400 mt-1">
            在飞书群「设置 -&gt; 群机器人 -&gt; 添加机器人 -&gt; 自定义机器人」中获取 Webhook 地址
          </p>
        </div>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <div>
            <label className="block text-xs font-medium text-slate-700 mb-1">
              安全签名密钥 (Secret，可选)
            </label>
            <Input
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              disabled={!canManage}
              placeholder="飞书/钉钉安全设置中的签名密钥（如无则留空）"
            />
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-700 mb-1">
              日报标题前缀
            </label>
            <Input
              value={titlePrefix}
              onChange={(e) => setTitlePrefix(e.target.value)}
              disabled={!canManage}
              placeholder="「数字图书馆」每日下载日报"
            />
          </div>
        </div>

        <div className="flex flex-wrap gap-6 py-2 border-y border-slate-100">
          <label className="flex items-center gap-2 cursor-pointer text-xs font-medium text-slate-700">
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => setEnabled(e.target.checked)}
              disabled={!canManage}
              className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
            />
            <span>启用每日定时自动推送</span>
          </label>

          <label className="flex items-center gap-2 cursor-pointer text-xs font-medium text-slate-700">
            <input
              type="checkbox"
              checked={includeSystemStatus}
              onChange={(e) => setIncludeSystemStatus(e.target.checked)}
              disabled={!canManage}
              className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
            />
            <span>在日报中附带 Worker 节点与账号资源池状态</span>
          </label>
        </div>

        {/* 手动即时发送 / 附言测试 */}
        <div className="rounded-lg bg-slate-50 p-4 border border-slate-200/80 space-y-3">
          <div className="flex items-center justify-between">
            <h4 className="text-xs font-semibold text-slate-800 flex items-center gap-1.5">
              <Send className="h-3.5 w-3.5 text-blue-600" />
              <span>立即自定义推送 / 连通测试</span>
            </h4>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowPreview(!showPreview)}
            >
              <Eye className="mr-1 h-3.5 w-3.5" />
              {showPreview ? "收起样例预览" : "查看今日日报 Markdown 预览"}
            </Button>
          </div>

          {showPreview && (
            <div className="rounded-md bg-white p-3 border border-slate-200 text-xs font-mono whitespace-pre-wrap text-slate-700 max-h-60 overflow-y-auto">
              {previewMarkdown}
            </div>
          )}

          <div className="flex gap-2">
            <Input
              value={customNote}
              onChange={(e) => setCustomNote(e.target.value)}
              placeholder="可选填本次即时推送附带的说明/备注..."
              className="text-xs"
            />
            <Button
              size="sm"
              onClick={handleManualSend}
              loading={sending}
              disabled={!url.trim()}
            >
              <Send className="mr-1 h-3.5 w-3.5" />
              立即推送今日数据
            </Button>
          </div>
        </div>

        {canManage && (
          <div className="flex justify-end gap-2 pt-2">
            <Button
              onClick={handleSave}
              loading={saving}
              className="px-6"
            >
              <CheckCircle2 className="mr-1.5 h-4 w-4" />
              保存推送配置
            </Button>
          </div>
        )}
      </div>
    </Card>
  );
}
