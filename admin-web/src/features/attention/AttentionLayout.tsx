import { NavLink, Outlet } from "react-router-dom";
import { KeyRound, FileCheck, Sparkles, AlertTriangle } from "lucide-react";

export function AttentionLayout() {
  const tabs = [
    { to: "/attention/manual", label: "人工验证", icon: KeyRound },
    { to: "/attention/inventory-reviews", label: "文件关联审核", icon: FileCheck },
    { to: "/attention/quality", label: "数据质量", icon: Sparkles },
    { to: "/attention/alerts", label: "系统告警", icon: AlertTriangle },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between border-b border-slate-200 pb-3">
        <div>
          <h1 className="text-xl font-bold text-slate-900">待处理事项</h1>
          <p className="text-sm text-slate-500">人工验证码决议、书目实体消歧与集群异常告警</p>
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
