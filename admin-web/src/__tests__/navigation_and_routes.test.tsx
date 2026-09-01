import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { NAV_GROUPS } from "../app/navigation";
import { isRoleAtLeast } from "../app/permissions";
import { RedirectWithId, RedirectWithQuery } from "../legacy/redirects";

function LocationProbe() {
  const location = useLocation();
  return <div>{location.pathname}{location.search}</div>;
}

describe("权威导航与兼容路由", () => {
  it("定义 9 个一级入口并只向超级管理员显示系统管理", () => {
    const items = NAV_GROUPS.flatMap((group) => group.items);
    expect(items).toHaveLength(9);
    const system = items.find((item) => item.to === "/system/logs");
    expect(system?.minRole).toBe("超级管理员");
    expect(isRoleAtLeast("任务管理员", system!.minRole!)).toBe(false);
    expect(isRoleAtLeast("超级管理员", system!.minRole!)).toBe(true);
  });

  it("旧检索路由重定向到图书总库并保留查询参数", async () => {
    render(
      <MemoryRouter initialEntries={["/catalog/search?q=test&language=zh"]} future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
        <Routes>
          <Route path="/catalog/search" element={<RedirectWithQuery to="/library" />} />
          <Route path="/library" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    await waitFor(() => expect(screen.getByText("/library?q=test&language=zh")).toBeInTheDocument());
  });

  it("旧详情路由把版本编号带到权威详情路径", async () => {
    render(
      <MemoryRouter initialEntries={["/catalog/editions/edition-1"]} future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
        <Routes>
          <Route path="/catalog/editions/:id" element={<RedirectWithId to="/library/editions" />} />
          <Route path="/library/editions/:id" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    await waitFor(() => expect(screen.getByText("/library/editions/edition-1")).toBeInTheDocument());
  });
});
