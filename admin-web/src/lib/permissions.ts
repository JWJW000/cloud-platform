// 权限矩阵（V5 方案第 5.4 节）。
//
// 规则：
// - 只读用户：只能看，不能写；
// - 任务管理员：可管理批次/任务/账号/代理，但不能审批节点、不能改系统设置；
// - 超级管理员：全部权限。
//
// 前端矩阵只是「入口隐藏 + 明确提示」，真正的强制校验在 Master 侧
// （require_super_admin / require_write），前端不可越权也不可静默。

import type { Role } from "./types";

export type Permission =
  | "approve_node" // 批准/拒绝/禁用节点（仅超级管理员）
  | "manage_batch" // 创建/开始/暂停/恢复/取消批次
  | "manage_task" // 任务重试/取消/确认
  | "manage_account" // 账号增删改
  | "manage_proxy" // 代理增删改
  | "manage_session" // 终止会话
  | "resolve_alert" // 处理告警
  | "manage_settings" // 修改系统设置
  | "view_all"; // 查看所有数据（三种角色都可）

const ROLE_PERMISSIONS: Record<Role, Permission[]> = {
  超级管理员: [
    "approve_node",
    "manage_batch",
    "manage_task",
    "manage_account",
    "manage_proxy",
    "manage_session",
    "resolve_alert",
    "manage_settings",
    "view_all",
  ],
  任务管理员: [
    "manage_batch",
    "manage_task",
    "manage_account",
    "manage_proxy",
    "manage_session",
    "view_all",
  ],
  只读用户: ["view_all"],
};

export function can(role: Role | undefined, permission: Permission): boolean {
  if (!role) return false;
  return ROLE_PERMISSIONS[role]?.includes(permission) ?? false;
}

/** 没有权限时的提示文案（禁止静默，V5 第 11.1 节）。 */
export function deniedMessage(permission: Permission): string {
  switch (permission) {
    case "approve_node":
      return "只有超级管理员可以批准或拒绝节点";
    case "manage_batch":
      return "需要管理员写权限（超级管理员或任务管理员）";
    case "manage_settings":
      return "只有超级管理员可以修改系统设置";
    default:
      return "当前角色没有该操作的权限";
  }
}
