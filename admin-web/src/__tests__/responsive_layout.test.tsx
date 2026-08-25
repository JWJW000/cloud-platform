import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

vi.mock("../context/AuthContext", () => ({
  useAuth: () => ({
    user: { id: "u1", username: "root", role: "超级管理员" },
    loading: false,
    logout: vi.fn(),
  }),
}));
vi.mock("../hooks/useSse", () => ({
  useSse: () => ({ state: "connected", lastEventAt: null, reconnectCount: 0 }),
}));

import { AppLayout } from "../components/layout";
import { EmptyRow, Table } from "../components/ui";

describe("移动布局与表格 DOM", () => {
  it("移动端提供有名称的抽屉菜单并包含 8 个入口", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter initialEntries={["/"]} future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
        <Routes>
          <Route element={<AppLayout />}>
            <Route index element={<div>总览内容</div>} />
          </Route>
        </Routes>
      </MemoryRouter>,
    );
    const open = screen.getByRole("button", { name: "打开导航菜单" });
    expect(open).toBeInTheDocument();
    await user.click(open);
    const dialog = screen.getByRole("dialog", { name: "导航菜单" });
    expect(dialog).toBeInTheDocument();
    expect(dialog.querySelectorAll("a")).toHaveLength(8);
    await user.click(screen.getByRole("button", { name: "关闭导航菜单" }));
    expect(screen.queryByRole("dialog", { name: "导航菜单" })).not.toBeInTheDocument();
    expect(open).toHaveFocus();
  });

  it("空状态行位于 tbody 内，不产生 tr/div 非法嵌套", () => {
    const { container } = render(
      <Table headers={["账号", "状态"]} empty={<EmptyRow colSpan={2} text="暂无账号" />}>
        {null}
      </Table>,
    );
    const row = screen.getByText("暂无账号").closest("tr");
    expect(row?.parentElement?.tagName).toBe("TBODY");
    expect(container.querySelector("div > tr")).toBeNull();
  });
});
