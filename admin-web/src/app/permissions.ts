import type { Role } from "../lib/types";

const ROLE_RANKS: Record<Role, number> = {
  超级管理员: 3,
  任务管理员: 2,
  只读用户: 1,
};

export function isRoleAtLeast(role: Role | undefined, minRole: Role): boolean {
  if (!role) return false;
  return (ROLE_RANKS[role] ?? 0) >= (ROLE_RANKS[minRole] ?? 0);
}
