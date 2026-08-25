import { NavLink, Outlet } from "react-router-dom";
import { Server, FolderSearch, Plug, Boxes } from "lucide-react";

export function OperationsLayout() {
  const tabs = [
    { to: "/operations/workers", label: "Worker 节点", icon: Server },
    { to: "/operations/inventory-scans", label: "馆藏扫描", icon: FolderSearch },
    { to: "/operations/proxies", label: "代理管理", icon: Plug },
    { to: "/operations/sessions", label: "执行会话", icon: Boxes },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between border-b border-slate-200 pb-3">
        <div>
          <h1 className="text-xl font-bold text-slate-900">运行资源</h1>
          <p className="text-sm text-slate-500">分布式 Worker 节点、网络代理池与活动会话监控</p>
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
