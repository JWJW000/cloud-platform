export type RegistrationPhase =
  | "pending"
  | "awaiting_email"
  | "extracting_code"
  | "submitting_code"
  | "manual_fallback"
  | "provider_error"
  | "completed"
  | "failed";

export interface PhaseInfo {
  label: string;
  color: string;
  badgeClass: string;
  description: string;
}

export const REGISTRATION_PHASE_CONFIG: Record<RegistrationPhase, PhaseInfo> = {
  pending: {
    label: "排队准备中",
    color: "slate",
    badgeClass: "bg-slate-100 text-slate-700",
    description: "任务已进入队列，等待分配 Worker 执行",
  },
  awaiting_email: {
    label: "等待邮件到达",
    color: "blue",
    badgeClass: "bg-blue-100 text-blue-800",
    description: "已提交注册表单，正在监听邮箱收取验证码邮件",
  },
  extracting_code: {
    label: "自动提取验证码",
    color: "amber",
    badgeClass: "bg-amber-100 text-amber-800",
    description: "已捕获新邮件，正在解析并提取验证码",
  },
  submitting_code: {
    label: "提交验证码",
    color: "indigo",
    badgeClass: "bg-indigo-100 text-indigo-800",
    description: "正在将提取到的验证码填入注册流程完成校验",
  },
  manual_fallback: {
    label: "人工降级处理中",
    color: "rose",
    badgeClass: "bg-rose-100 text-rose-800",
    description: "自动取码不可用或超时，已触发人工验证事项",
  },
  provider_error: {
    label: "Provider 异常",
    color: "red",
    badgeClass: "bg-red-100 text-red-800",
    description: "邮件服务连接或认证异常",
  },
  completed: {
    label: "注册完成",
    color: "emerald",
    badgeClass: "bg-emerald-100 text-emerald-800",
    description: "账号注册成功并可用",
  },
  failed: {
    label: "注册失败",
    color: "red",
    badgeClass: "bg-red-100 text-red-800",
    description: "尝试次数耗尽或发生不可恢复错误",
  },
};

export function parseTaskPhase(status: string, reason?: string | null): RegistrationPhase {
  if (status === "已完成" || status === "成功") return "completed";
  if (status === "失败" || status === "已取消") return "failed";

  const text = (reason || "").toLowerCase();
  if (text.includes("人工") || text.includes("manual")) return "manual_fallback";
  if (text.includes("提取") || text.includes("parsing")) return "extracting_code";
  if (text.includes("等待") || text.includes("email") || text.includes("mail")) return "awaiting_email";
  if (text.includes("提交") || text.includes("submit")) return "submitting_code";
  if (text.includes("provider") || text.includes("异常") || text.includes("error")) return "provider_error";

  if (status === "运行中") return "awaiting_email";
  return "pending";
}
