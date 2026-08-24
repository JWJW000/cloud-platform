// 权限矩阵测试：不同角色看到的菜单与按钮不同；无权限不静默（V5 第 11.1 节）。
import { describe, expect, it } from "vitest";
import { can, deniedMessage, type Permission } from "../lib/permissions";

const ALL_PERMISSIONS: Permission[] = [
  "approve_node",
  "manage_batch",
  "manage_task",
  "manage_account",
  "manage_proxy",
  "manage_session",
  "resolve_alert",
  "manage_settings",
];

describe("权限矩阵", () => {
  it("超级管理员拥有全部权限", () => {
    for (const p of ALL_PERMISSIONS) {
      expect(can("超级管理员", p), `缺少权限 ${p}`).toBe(true);
    }
  });

  it("任务管理员可管理批次/任务/账号/代理/会话，但不能审批节点和改设置", () => {
    expect(can("任务管理员", "manage_batch")).toBe(true);
    expect(can("任务管理员", "manage_task")).toBe(true);
    expect(can("任务管理员", "manage_account")).toBe(true);
    expect(can("任务管理员", "manage_proxy")).toBe(true);
    expect(can("任务管理员", "manage_session")).toBe(true);
    expect(can("任务管理员", "approve_node")).toBe(false);
    expect(can("任务管理员", "manage_settings")).toBe(false);
  });

  it("只读用户只能查看", () => {
    for (const p of ALL_PERMISSIONS) {
      expect(can("只读用户", p), `只读用户不应拥有 ${p}`).toBe(false);
    }
  });

  it("未登录（无角色）无任何权限", () => {
    expect(can(undefined, "view_all")).toBe(false);
    expect(can(undefined, "manage_batch")).toBe(false);
  });

  it("无权限时给出明确中文提示（禁止静默）", () => {
    expect(deniedMessage("approve_node")).toContain("超级管理员");
    expect(deniedMessage("manage_settings")).toContain("超级管理员");
    expect(deniedMessage("manage_batch")).toContain("管理员");
  });
});
