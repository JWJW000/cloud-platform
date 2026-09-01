import {
  Gauge,
  BookOpen,
  Building2,
  DownloadCloud,
  UploadCloud,
  Users,
  Server,
  AlertTriangle,
  Settings,
  type LucideIcon,
} from "lucide-react";
import type { Role } from "../lib/types";

export interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
  end?: boolean;
  minRole?: Role;
}

export interface NavGroup {
  groupName: string;
  items: NavItem[];
}

export const NAV_GROUPS: NavGroup[] = [
  {
    groupName: "我的图书",
    items: [
      { to: "/", label: "总览", icon: Gauge, end: true },
      { to: "/library", label: "我的书目总库", icon: BookOpen },
      { to: "/publishers", label: "出版社管理", icon: Building2 },
      { to: "/acquisitions", label: "获取任务", icon: DownloadCloud },
      { to: "/imports", label: "数据导入", icon: UploadCloud },
    ],
  },
  {
    groupName: "运行",
    items: [
      { to: "/accounts", label: "账号中心", icon: Users },
      { to: "/operations/workers", label: "运行资源", icon: Server },
      { to: "/attention/manual", label: "待处理", icon: AlertTriangle },
    ],
  },
  {
    groupName: "管理",
    items: [
      { to: "/system/logs", label: "系统管理", icon: Settings, minRole: "超级管理员" },
    ],
  },
];
