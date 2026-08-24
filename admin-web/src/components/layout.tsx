// 管理后台布局：左侧导航 + 顶栏（用户、SSE 实时状态、退出）。
import { NavLink, Outlet, useNavigate } from "react-router-dom";
import {
  AlertTriangle,
  BookOpen,
  Boxes,
  ClipboardList,
  Gauge,
  KeyRound,
  Layers,
  ListChecks,
  LogOut,
  Radio,
  Server,
  Settings,
  Shield,
  UserCheck,
  Users,
  Wifi,
  WifiOff,
  RefreshCw,
  Plug,
} from "lucide-react";
import { useAuth } from "../context/AuthContext";
import { useSse, type SseState } from "../hooks/useSse";
import { can } from "../lib/permissions";

const NAV = [
  { to: "/", label: "总览", icon: Gauge, end: true },
  { to: "/workers", label: "Worker 节点", icon: Server },
  { to: "/batches", label: "下载批次", icon: Layers },
  { to: "/account-registration-batches", label: "账号注册批次", icon: UserCheck },
  { to: "/manual-actions", label: "待确认事项", icon: KeyRound },
  { to: "/books", label: "图书主数据", icon: BookOpen },
  { to: "/tasks", label: "任务", icon: ListChecks },
  { to: "/accounts", label: "下载账号", icon: Users },
  { to: "/proxies", label: "代理", icon: Plug },
  { to: "/sessions", label: "执行会话", icon: Boxes },
  { to: "/alerts", label: "告警", icon: AlertTriangle },
  { to: "/logs", label: "操作日志", icon: ClipboardList },
  { to: "/settings", label: "系统设置", icon: Settings },
];

const SSE_LABEL: Record<SseState, string> = {
  connecting: "连接中",
  connected: "实时已连接",
  reconnecting: "重连中",
  disconnected: "已断开",
};

function SseBadge({ state }: { state: SseState }) {
  const icon =
    state === "connected" ? (
      <Wifi className="h-3 w-3" />
    ) : state === "reconnecting" ? (
      <RefreshCw className="h-3 w-3 animate-spin" />
    ) : (
      <WifiOff className="h-3 w-3" />
    );
  const color =
    state === "connected"
      ? "bg-green-100 text-green-700"
      : state === "reconnecting"
        ? "bg-amber-100 text-amber-700"
        : state === "connecting"
          ? "bg-blue-100 text-blue-700"
          : "bg-slate-200 text-slate-500";
  return (
    <span
      title="SSE 实时事件流状态"
      className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium ${color}`}
    >
      {icon}
      {SSE_LABEL[state]}
    </span>
  );
}

export function AppLayout() {
  const { user, logout } = useAuth();
  const navigate = useNavigate();
  const sse = useSse(!!user);

  const handleLogout = async () => {
    await logout();
    navigate("/login");
  };

  return (
    <div className="flex h-full min-h-screen">
      {/* 侧栏 */}
      <aside className="flex w-56 shrink-0 flex-col border-r border-slate-200 bg-white">
        <div className="flex items-center gap-2 border-b border-slate-100 px-4 py-4">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-blue-600 text-white">
            <Radio className="h-4 w-4" />
          </span>
          <div>
            <div className="text-sm font-bold text-slate-900">Drission Cloud</div>
            <div className="text-[11px] text-slate-400">云端调度管理平台</div>
          </div>
        </div>
        <nav className="flex-1 space-y-0.5 overflow-y-auto p-2">
          {NAV.map(({ to, label, icon: Icon, end }) => (
            <NavLink
              key={to}
              to={to}
              end={end}
              className={({ isActive }) =>
                `flex items-center gap-2.5 rounded-md px-3 py-2 text-sm transition ${
                  isActive
                    ? "bg-blue-50 font-medium text-blue-700"
                    : "text-slate-600 hover:bg-slate-50"
                }`
              }
            >
              <Icon className="h-4 w-4" />
              {label}
            </NavLink>
          ))}
        </nav>
        <div className="border-t border-slate-100 p-3">
          <div className="flex items-center gap-2 rounded-md px-2 py-1.5 text-xs text-slate-500">
            <Shield className="h-3.5 w-3.5" />
            {user?.role ?? "-"}
          </div>
        </div>
      </aside>

      {/* 主区 */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center justify-between border-b border-slate-200 bg-white px-5">
          <div className="text-sm text-slate-500">
            你好，<span className="font-medium text-slate-800">{user?.username}</span>
          </div>
          <div className="flex items-center gap-3">
            <SseBadge state={sse.state} />
            <button
              onClick={handleLogout}
              className="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm text-slate-500 hover:bg-slate-100 hover:text-slate-700"
            >
              <LogOut className="h-4 w-4" />
              退出
            </button>
          </div>
        </header>
        <main className="flex-1 overflow-y-auto p-5">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

/** 只读用户提示条（放在写操作页面顶部，不静默）。 */
export function ReadonlyBanner() {
  const { user } = useAuth();
  if (can(user?.role, "manage_batch")) return null;
  return (
    <div className="mb-4 flex items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-4 py-2.5 text-sm text-amber-800">
      <AlertTriangle className="h-4 w-4" />
      当前为只读账号：只能查看数据，所有写操作不可用。
    </div>
  );
}
