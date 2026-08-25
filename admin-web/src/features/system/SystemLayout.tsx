import { NavLink, Outlet } from "react-router-dom";
import { ClipboardList, Settings } from "lucide-react";

export function SystemLayout() {
  const tabs = [
    { to: "/system/logs", label: "操作日志", icon: ClipboardList },
    { to: "/system/settings", label: "系统设置", icon: Settings },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between border-b border-slate-200 pb-3">
        <div>
          <h1 className="text-xl font-bold text-slate-900">系统管理</h1>
          <p className="text-sm text-slate-500">审计日志、安全策略与集群全局运行参数配置</p>
        </div>
        <nav className="flex space-x-1 rounded-lg bg-slate-100 p-1">
          {tabs.map(({ to, label, icon: Icon }) => (
            <NavLink
              key={to}
              to={to}
              className={({ isActive }) =>
                `flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition ${
                  isActive
                    ? "bg-white text-blue-700 shadow-sm"
                    : "text-slate-600 hover:text-slate-900"
                }`
              }
            >
              <Icon className="h-4 w-4" />
              {label}
            </NavLink>
          ))}
        </nav>
      </div>
      <div>
        <Outlet />
      </div>
    </div>
  );
}
