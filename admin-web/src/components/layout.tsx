// 管理后台布局：3 大分组、8 个权威导航入口 + 顶栏（用户、SSE 实时状态、移动端抽屉、退出）。
import { useState, useRef, useEffect } from "react";
import { NavLink, Outlet, useNavigate, useLocation } from "react-router-dom";
import {
  LogOut,
  Shield,
  WifiOff,
  RefreshCw,
  Menu,
  X,
} from "lucide-react";
import { useAuth } from "../context/AuthContext";
import { useSse, type SseState } from "../hooks/useSse";
import { NAV_GROUPS } from "../app/navigation";
import { isRoleAtLeast } from "../app/permissions";

const SSE_LABEL: Record<SseState, string> = {
  connecting: "连接中",
  connected: "实时已连接",
  reconnecting: "重连中",
  disconnected: "已断开",
};

function SseBadge({ state }: { state: SseState }) {
  // 正常连接时不常驻显示，只在连接中、重连中和已断开时显示提示
  if (state === "connected") {
    return null;
  }

  const icon =
    state === "reconnecting" ? (
      <RefreshCw className="h-3 w-3 animate-spin" />
    ) : (
      <WifiOff className="h-3 w-3" />
    );
  const color =
    state === "reconnecting"
      ? "bg-amber-100 text-amber-700"
      : state === "connecting"
        ? "bg-blue-100 text-blue-700"
        : "bg-slate-200 text-slate-500";

  return (
    <span
      role="status"
      aria-live="polite"
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
  const location = useLocation();
  const sse = useSse(!!user);
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const menuButtonRef = useRef<HTMLButtonElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);
  const wasMobileMenuOpen = useRef(false);

  const handleLogout = async () => {
    await logout();
    navigate("/login");
  };

  // 路由切换时自动关闭抽屉
  useEffect(() => {
    setMobileMenuOpen(false);
  }, [location.pathname]);

  // 打开抽屉时管理焦点，关闭后返回菜单按钮
  useEffect(() => {
    if (mobileMenuOpen) {
      wasMobileMenuOpen.current = true;
      drawerRef.current?.focus();
    } else if (wasMobileMenuOpen.current && menuButtonRef.current) {
      wasMobileMenuOpen.current = false;
      menuButtonRef.current.focus();
    }
  }, [mobileMenuOpen]);

  useEffect(() => {
    if (!mobileMenuOpen) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMobileMenuOpen(false);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [mobileMenuOpen]);

  const renderNavGroups = (onClickItem?: () => void) => (
    <nav className="flex-1 space-y-4 overflow-y-auto p-3">
      {NAV_GROUPS.map((group) => {
        const visibleItems = group.items.filter(
          (item) => !item.minRole || isRoleAtLeast(user?.role, item.minRole)
        );
        if (visibleItems.length === 0) return null;

        return (
          <div key={group.groupName} className="space-y-1">
            <div className="px-2.5 text-[11px] font-semibold text-slate-400">
              {group.groupName}
            </div>
            {visibleItems.map(({ to, label, icon: Icon, end }) => (
              <NavLink
                key={to}
                to={to}
                end={end}
                onClick={onClickItem}
                className={({ isActive }) =>
                  `flex items-center gap-2.5 rounded-md px-2.5 py-1.5 text-sm transition ${
                    isActive
                      ? "bg-blue-50 font-medium text-blue-700"
                      : "text-slate-600 hover:bg-slate-50 hover:text-slate-900"
                  }`
                }
              >
                <Icon className="h-4 w-4 shrink-0" />
                <span>{label}</span>
              </NavLink>
            ))}
          </div>
        );
      })}
    </nav>
  );

  return (
    <div className="flex h-full min-h-screen">
      {/* 桌面端侧栏 */}
      <aside className="hidden md:flex w-56 shrink-0 flex-col border-r border-slate-200 bg-white">
        <div className="flex items-center gap-2 border-b border-slate-100 px-4 py-4">
          <img src="/favicon.png" alt="Logo" className="h-8 w-8 rounded-lg object-contain" />
          <div>
            <div className="text-sm font-bold text-slate-900">Drission Cloud</div>
            <div className="text-[11px] text-slate-400">数字图书馆总库调度</div>
          </div>
        </div>

        {/* 3 分组导航 */}
        {renderNavGroups()}

        <div className="border-t border-slate-100 p-3">
          <div className="flex items-center gap-2 rounded-md px-2 py-1.5 text-xs text-slate-500">
            <Shield className="h-3.5 w-3.5 text-slate-400" />
            <span className="truncate">{user?.role ?? "-"}</span>
          </div>
        </div>
      </aside>

      {/* 移动端抽屉导航 */}
      {mobileMenuOpen && (
        <div className="fixed inset-0 z-50 md:hidden flex">
          {/* 背景蒙层 */}
          <div
            className="fixed inset-0 bg-slate-900/40 transition-opacity"
            aria-hidden="true"
            onClick={() => setMobileMenuOpen(false)}
          />
          {/* 抽屉内容 */}
          <div
            ref={drawerRef}
            tabIndex={-1}
            role="dialog"
            aria-modal="true"
            aria-label="导航菜单"
            className="relative flex w-64 max-w-xs flex-1 flex-col bg-white shadow-xl focus:outline-none"
          >
            <div className="flex items-center justify-between border-b border-slate-100 px-4 py-4">
              <div className="flex items-center gap-2">
                <img src="/favicon.png" alt="Logo" className="h-8 w-8 rounded-lg object-contain" />
                <div className="text-sm font-bold text-slate-900">Drission Cloud</div>
              </div>
              <button
                type="button"
                onClick={() => setMobileMenuOpen(false)}
                aria-label="关闭导航菜单"
                className="rounded-md p-1.5 text-slate-400 hover:bg-slate-100 hover:text-slate-600 focus:outline-none focus:ring-2 focus:ring-blue-500"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {renderNavGroups(() => setMobileMenuOpen(false))}

            <div className="border-t border-slate-100 p-3">
              <div className="flex items-center gap-2 rounded-md px-2 py-1.5 text-xs text-slate-500">
                <Shield className="h-3.5 w-3.5 text-slate-400" />
                <span>{user?.role ?? "-"}</span>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* 主区 */}
      <div className="flex min-w-0 flex-1 flex-col h-full overflow-hidden">
        <header className="flex h-14 shrink-0 items-center justify-between border-b border-slate-200 bg-white px-4 sm:px-5">
          <div className="flex items-center gap-3">
            <button
              ref={menuButtonRef}
              type="button"
              onClick={() => setMobileMenuOpen(true)}
              aria-label="打开导航菜单"
              className="inline-flex md:hidden items-center justify-center rounded-md p-1.5 text-slate-500 hover:bg-slate-100 hover:text-slate-700 focus:outline-none focus:ring-2 focus:ring-blue-500"
            >
              <Menu className="h-5 w-5" />
            </button>
            <div className="text-sm text-slate-500">
              你好，<span className="font-medium text-slate-800">{user?.username}</span>
            </div>
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
        <main className="flex-1 min-h-0 overflow-y-auto p-4 sm:p-5">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
