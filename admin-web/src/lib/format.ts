// 展示辅助：状态中文映射、时间格式化、字节格式化。
import type { Dict } from "./types";

/** 字节数 → 人类可读。 */
export function formatBytes(bytes: number | null | undefined): string {
  if (!bytes || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}

/** RFC3339 → 本地时间字符串（空值显示 -）。 */
export function formatTime(iso: string | null | undefined): string {
  if (!iso) return "-";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "-";
  return d.toLocaleString("zh-CN", { hour12: false });
}

/** 相对时间（xx 分钟前）。 */
export function relativeTime(iso: string | null | undefined): string {
  if (!iso) return "-";
  const d = new Date(iso).getTime();
  if (Number.isNaN(d)) return "-";
  const diff = Date.now() - d;
  const minutes = Math.floor(diff / 60000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  return `${days} 天前`;
}

/** 短 ID（前 8 位）。 */
export function shortId(id: string | null | undefined): string {
  if (!id) return "-";
  return id.length > 8 ? id.slice(0, 8) : id;
}

/** 由字典生成「值 → 中文」映射；未知值原样返回（不做英文硬编码）。 */
export function dictLabels(dict: Dict | null, key: keyof Dict): Record<string, string> {
  const values = dict?.[key] ?? [];
  const map: Record<string, string> = {};
  for (const v of values) map[v] = v;
  return map;
}

/** 状态徽章配色（中文状态 → Tailwind class）。 */
export function statusColor(status: string | undefined): string {
  if (!status) return "bg-slate-100 text-slate-600";
  if (status.includes("在线") || status.includes("完成") || status.includes("已批准") || status === "已确认" || status.includes("已注册"))
    return "bg-green-100 text-green-700";
  if (status.includes("忙碌") || status.includes("执行") || status.includes("运行") || status.includes("已分配") || status === "待开始" || status.includes("待处理") || status.includes("待审核") || status.includes("待确认") || status.includes("等待"))
    return "bg-amber-100 text-amber-700";
  if (status.includes("失败") || status.includes("错误") || status.includes("异常") || status.includes("离线") || status.includes("已禁用") || status.includes("已拒绝") || status.includes("已过期") || status.includes("已取消"))
    return "bg-red-100 text-red-700";
  if (status.includes("暂停") || status.includes("维护") || status.includes("断线") || status.includes("冷却"))
    return "bg-slate-200 text-slate-700";
  return "bg-blue-100 text-blue-700";
}
